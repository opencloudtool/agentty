//! Session transcript rendering and scrolling.

use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::region::Region;

use super::fixture::E2eResult;
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

const LOADER_SESSION_ID: &str = "loader-session-0001";

/// Returns every scrollbar row and the subset occupied by its thumb in the
/// session output's rightmost column.
fn session_output_scrollbar_rows(frame: &TerminalFrame) -> (Vec<u16>, Vec<u16>) {
    let scrollbar_column = frame.cols().saturating_sub(2);
    let mut scrollbar_rows = Vec::new();
    let mut thumb_rows = Vec::new();

    for row in 0..frame.rows() {
        match frame.cell_text(row, scrollbar_column) {
            "█" => {
                scrollbar_rows.push(row);
                thumb_rows.push(row);
            }
            "│" => scrollbar_rows.push(row),
            _ => {}
        }
    }

    (scrollbar_rows, thumb_rows)
}

/// Seeds one review-ready session whose transcript contains a beautified
/// provider command failure.
fn seed_session_with_beautified_agent_error(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("agent-error-0001", "claude-opus-5", "main", "Review")
            .with_title("Readable agent error"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "agent-error-0001",
                SessionMessageKind::AssistantAnswer,
                "\
Agent command failed with exit code 1.

stdout:
```text
system | init | cwd: /tmp/test-agentty/wt/d4ab835d
proxy warning: retrying
rate_limit_event | rate_limit_status: rejected | rate_limit_reason: out_of_credits
assistant: hi
result error: rate_limit
message: You've hit your session limit - resets 12:10am (America/Los_Angeles)
api error status: 429
request id: req_011Cbfc7AF16gbH
duration: 283ms
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("agent-er"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains a markdown table.
fn seed_session_with_markdown_table(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("markdown-table-0001", "claude-opus-5", "main", "Review")
            .with_title("Markdown table output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "markdown-table-0001",
                SessionMessageKind::AssistantAnswer,
                "\
| Message kind | Storage |
| --- | --- |
| User prompt | Session.output |
| Assistant markdown | session_message |
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("markdown"))?;

    Ok(())
}

/// Seeds one review-ready session whose user prompt contains markdown.
fn seed_session_with_user_markdown(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("user-markdown-0001", "claude-opus-5", "main", "Review")
            .with_title("User markdown prompt"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "user-markdown-0001",
                SessionMessageKind::UserPrompt,
                "\
Use **bold** and `code`. Review @crates/agentty/src/ui/markdown.rs.

| Input | Meaning |
| --- | --- |
| User prompt | Markdown |

```text
formatted blocks in user messages without words breaking
```

```mermaid {theme=default}
flowchart TD
    A[Start] --> B[Finish]
```
",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "user-markdown-0001",
                SessionMessageKind::AssistantAnswer,
                "Assistant answer after the user markdown prompt.",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("user-mar"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains inline markdown
/// styling adjacent to punctuation.
fn seed_session_with_inline_markdown_punctuation(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("inline-md-0001", "claude-opus-5", "main", "Review")
            .with_title("Inline markdown punctuation"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "inline-md-0001",
                SessionMessageKind::AssistantAnswer,
                "Use (`session_messages_from_rows`), then [`Image #1`].\n",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("inline-m"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains inline right-arrow
/// math syntax.
fn seed_session_with_inline_math(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("inline-math-0001", "claude-opus-5", "main", "Review")
            .with_title("Inline math output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "inline-math-0001",
                SessionMessageKind::AssistantAnswer,
                r"Continue $\rightarrow$, then **$\rightarrow$** and *$\rightarrow$*.
Display $$text **$\rightarrow$** and *$\rightarrow$* text$$ literally.
Code **`$\rightarrow$`** and *`$\rightarrow$`* literally.",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("inline-m"))?;

    Ok(())
}

/// Seeds one review-ready session whose transcript contains mermaid flowchart,
/// entity-relationship, and sequence fenced blocks. The flowchart includes an
/// extended shape, an `&` fan-out, and bidirectional arrows, while the sequence
/// diagram includes a skipped control block.
fn seed_session_with_mermaid_output(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("mermaid-chat-0001", "claude-opus-5", "main", "Review")
            .with_title("Mermaid diagram output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "mermaid-chat-0001",
                SessionMessageKind::AssistantAnswer,
                "\
Here is the merge flow and the data model:

```mermaid
flowchart TD
    A{{User starts session}} --> B{Choose action}
    B -->|Route request| C[Send prompt] & D[Open diff view]
    C <--> E[Agent works in worktree]
    E --> F[Run checks]
    F <-- G[Report result]
    D --> G
```

```mermaid
erDiagram
    CUSTOMER ||--o{ ORDER : places
    CUSTOMER ||--|| ACCOUNT : owns
```

```mermaid
sequenceDiagram
    participant User
    participant Agentty
    participant Agent
    User->>Agentty: Start new session
    alt Agent available
    Agentty->>Agent: Send prompt
    Agent-->>Agentty: Stream result
    end
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("mermaid-"))?;

    Ok(())
}

/// Seeds one review-ready session with the cyclic orchestration flow that
/// previously fell back to a plain code block.
fn seed_session_with_cyclic_mermaid_output(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("cyclic-mermaid-0001", "claude-opus-5", "main", "Review")
            .with_title("Cyclic Mermaid output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "cyclic-mermaid-0001",
                SessionMessageKind::AssistantAnswer,
                "\
```mermaid
flowchart LR
    U[User and TUI] --> C[Orchestrator controller]
    M[Agent model] --> P[Typed command response]
    P --> C
    C --> S[ag-session service]
    S --> A[Agentty host adapter]
    A --> W[Session workers]
    W --> E[Session events]
    E --> C
    C --> M
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("cyclic-m"))?;

    Ok(())
}

/// Seeds one review-ready session with a left-to-right telemetry flow that is
/// wider than its session output panel and must use the compact layout.
fn seed_session_with_compact_mermaid_output(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("compact-mermaid-0001", "qwen3-coder-plus", "main", "Review")
            .with_title("Compact Mermaid output"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "compact-mermaid-0001",
                SessionMessageKind::AssistantAnswer,
                "\
```mermaid
flowchart LR
    Q[Qwen complete] --> T[Tracing spans and events]
    Q --> M[OTel metrics API]
    T --> S[Trace and log providers]
    M --> P[Meter provider]
    S --> O[OTLP HTTP protobuf]
    P --> O
    O --> C[Collector on port 4318]
    C --> B[Telemetry backends]
    B --> G[Grafana on port 3000]
```
",
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("compact-"))?;

    Ok(())
}

/// Seeds one review-ready session whose assistant answer begins a line with a
/// workflow-notice prefix that must remain assistant text.
fn seed_session_with_typed_marker_collision(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = "typed-marker-0001";
    common::seed_session(
        env,
        SessionSeed::regular(session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Typed marker collision"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::UserPrompt,
                "explain merge label output",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::AssistantAnswer,
                "Assistant output before marker.\n[Merge] this is literal assistant text.",
            )
            .await
    })?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        session_id,
    ))?;

    Ok(())
}

/// Seeds one in-progress session so the session view can show the active
/// Tachyonfx loader without launching a live agent backend.
fn seed_active_loader_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(LOADER_SESSION_ID, "gpt-5.6-sol", "main", "InProgress")
            .with_title("Loader session"),
    )?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        LOADER_SESSION_ID,
    ))?;

    Ok(())
}

/// Seeds one review-ready session with enough output to overflow a compact
/// transcript viewport.
fn seed_session_with_scrollable_output(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    const SESSION_ID: &str = "scroll-output-0001";

    common::seed_session(
        env,
        SessionSeed::regular(SESSION_ID, "gpt-5.6-sol", "main", "Review")
            .with_title("Scrollable output"),
    )?;

    let output = (0..60)
        .map(|line_index| format!("Transcript line {line_index:02} {}", "x".repeat(60)))
        .collect::<Vec<_>>()
        .join("\n");
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(SESSION_ID, SessionMessageKind::AssistantAnswer, &output)
            .await
    })?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        SESSION_ID,
    ))?;

    Ok(())
}

/// Verify that session output renders beautified provider command failures
/// with readable JSONL event summaries instead of raw event payloads.
#[test]
fn session_view_agent_error_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_view_agent_error_output")
        .setup(seed_session_with_beautified_agent_error)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("result error: rate_limit", 5000)
                    .capture_labeled(
                        "agent_error",
                        "Session view with beautified agent command error output",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "proxy warning: retrying", &full);
                assertion::assert_text_in_region(frame, "result error: rate_limit", &full);
                assertion::assert_text_in_region(
                    frame,
                    "message: You've hit your session limit",
                    &full,
                );
                assertion::assert_text_in_region(frame, "request id: req_011Cbfc7AF16gbH", &full);
            },
        )?;

    Ok(())
}

/// Verify that session output renders markdown pipe tables as aligned terminal
/// tables instead of showing the raw separator row.
#[test]
fn session_view_markdown_table_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_markdown_table_output")
        .setup(seed_session_with_markdown_table)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Session.output", 5000)
                    .capture_labeled(
                        "markdown_table",
                        "Session view with a rendered markdown table",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Message kind", &full);
                assertion::assert_text_in_region(frame, "Assistant markdown", &full);
                assertion::assert_text_in_region(frame, "Session.output", &full);
                assertion::assert_not_visible(frame, "| --- | --- |");
            },
        )?;

    Ok(())
}

/// Verify that markdown in user prompts renders like markdown output while
/// retaining the visible prompt marker.
#[test]
fn session_view_user_prompt_markdown_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_user_prompt_markdown_output")
        .setup(seed_session_with_user_markdown)
        // Keep the entire markdown fixture visible below the session header.
        .with_terminal_size(80, 40)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("User prompt", 5000)
                    .capture_labeled(
                        "user_prompt_markdown",
                        "Session view with rendered markdown in a user prompt",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "@crates/agentty/src/ui/markdown.rs",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Use bold and code.", &full);
                assertion::assert_text_in_region(frame, "User prompt", &full);
                assertion::assert_text_in_region(frame, "Markdown", &full);
                assertion::assert_text_in_region(frame, "without words breaking", &full);
                assertion::assert_text_in_region(frame, "Start", &full);
                assertion::assert_text_in_region(frame, "Finish", &full);
                assertion::assert_text_in_region(frame, "▼", &full);
                assertion::assert_not_visible(frame, "**bold**");
                assertion::assert_not_visible(frame, "`code`");
                assertion::assert_not_visible(frame, "| --- | --- |");
                assertion::assert_not_visible(frame, "flowchart TD");
            },
        )?;

    Ok(())
}

/// Verify that reopening a cached session after a theme switch repaints
/// transcript messages with the newly active theme.
#[test]
fn session_view_theme_switch_repaints_cached_messages() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_theme_switch_repaints_cached_messages")
        .setup(seed_session_with_user_markdown)
        // Cache and repaint the same visible messages across the theme switch.
        .with_terminal_size(80, 40)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Use bold and code.", 5000)
                    .press_key("q")
                    .wait_for_text("User markdown prompt", 5000)
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Green", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Dark Horizon", 5000)
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("User markdown prompt", 5000)
                    .press_key("Enter")
                    .wait_for_text("Use bold and code.", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Use bold and code.", &full);
            },
        )?;

    Ok(())
}

/// Verify that inline markdown styling adjacent to punctuation does not add
/// spaces inside brackets or parentheses.
#[test]
fn session_view_inline_markdown_punctuation_spacing() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_inline_markdown_punctuation_spacing")
        .setup(seed_session_with_inline_markdown_punctuation)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Image #1]", 5000)
                    .capture_labeled(
                        "inline_markdown_punctuation",
                        "Session view with inline markdown punctuation spacing",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Use (session_messages_from_rows), then [Image #1].",
                    &full,
                );
                assertion::assert_not_visible(frame, "( session_messages_from_rows )");
                assertion::assert_not_visible(frame, "[ Image #1 ]");
            },
        )?;

    Ok(())
}

/// Verify that inline right-arrow math syntax renders as a Unicode arrow in
/// session chat.
#[test]
fn session_view_inline_right_arrow_math() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_inline_right_arrow_math")
        .setup(seed_session_with_inline_math)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Continue →, then → and →.", 5000)
                    .wait_for_text(
                        r"Display $$text **$\rightarrow$** and *$\rightarrow$* text$$ literally.",
                        5000,
                    )
                    .wait_for_text(r"Code `$\rightarrow$` and `$\rightarrow$` literally.", 5000)
                    .capture_labeled(
                        "inline_right_arrow_math",
                        "Session view with rendered inline right-arrow math",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Continue →, then → and →.", &full);
                assertion::assert_text_in_region(
                    frame,
                    r"Display $$text **$\rightarrow$** and *$\rightarrow$* text$$ literally.",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    r"Code `$\rightarrow$` and `$\rightarrow$` literally.",
                    &full,
                );
                assertion::assert_not_visible(frame, r"Continue $\rightarrow$");
            },
        )?;

    Ok(())
}

/// Verify that typed assistant output is not reclassified as a workflow notice
/// just because it starts a line with a notice-looking prefix.
#[test]
fn session_view_preserves_typed_assistant_marker_lines() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_typed_assistant_marker_lines")
        .setup(seed_session_with_typed_marker_collision)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Merge] this is literal assistant text.", 5000)
                    .capture_labeled(
                        "typed_assistant_marker",
                        "Typed assistant line that looks like a workflow notice",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);
                assertion::assert_text_in_region(
                    frame,
                    "[Merge] this is literal assistant text.",
                    &full,
                );
                assert!(!view_text.contains("Change Summary"));
            },
        )?;

    Ok(())
}

/// Verify that active session output uses the Tachyonfx loader glyph instead
/// of dot-based working copy.
#[test]
fn session_active_loader_uses_tachyonfx_glyph() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_active_loader")
        .setup(seed_active_loader_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_active_loader",
                        "Active session view with Tachyonfx loader",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "▌▌▌ Working...", &full);
            },
        )?;

    Ok(())
}

/// Verify that overflowing session output shows a scrollbar in the panel's
/// rightmost column and returning to the list clears the chat page.
#[test]
fn session_output_scrollbar_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_output_scrollbar")
        .with_git()
        .with_terminal_size(80, 20)
        .setup(seed_session_with_scrollable_output)
        .zola(
            "Session output scrollbar",
            "Track your position while scrolling through long session transcripts.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("g")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_output_scrollbar_top",
                        "Scrollbar thumb at the top of long session output",
                    )
                    .write_text("G")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_output_scrollbar_bottom",
                        "Scrollbar thumb at the bottom of long session output",
                    )
                    .compose(&common::return_to_session_list())
                    .capture_labeled(
                        "session_list_after_scrollbar",
                        "Session list after leaving scrolled session output",
                    )
            },
            |_frame, report| {
                assert_eq!(report.captures.len(), 3);

                let top_frame = common::frame_from_capture(&report.captures[0]);
                let bottom_frame = common::frame_from_capture(&report.captures[1]);
                let list_frame = common::frame_from_capture(&report.captures[2]);
                let (top_scrollbar_rows, top_thumb_rows) =
                    session_output_scrollbar_rows(&top_frame);
                let (bottom_scrollbar_rows, bottom_thumb_rows) =
                    session_output_scrollbar_rows(&bottom_frame);
                let scrollbar_padding_column = top_frame.cols().saturating_sub(3);

                assert!(top_scrollbar_rows.len() > top_thumb_rows.len());
                assert!(bottom_scrollbar_rows.len() > bottom_thumb_rows.len());
                assert!(
                    top_scrollbar_rows
                        .iter()
                        .all(|row| { top_frame.cell_text(*row, scrollbar_padding_column) == " " })
                );
                assert!(
                    bottom_scrollbar_rows.iter().all(|row| {
                        bottom_frame.cell_text(*row, scrollbar_padding_column) == " "
                    })
                );
                assert_eq!(top_thumb_rows.first(), top_scrollbar_rows.first());
                assert_eq!(bottom_thumb_rows.last(), bottom_scrollbar_rows.last());
                assert!(
                    top_thumb_rows.last() < bottom_thumb_rows.first(),
                    "expected the scrollbar thumb to move from top to bottom"
                );
                assert!(
                    list_frame
                        .text_in_region(&Region::full(list_frame.cols(), list_frame.rows()))
                        .chars()
                        .all(|character| character != '█'),
                    "expected no stale scrollbar thumb after returning to the session list"
                );
            },
        )?;

    Ok(())
}

/// Verify session output renders mermaid flowchart, entity-relationship, and
/// sequence fenced blocks as Unicode diagrams instead of raw mermaid source.
#[test]
fn session_view_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_mermaid_output")
        .with_terminal_size(160, 72)
        .setup(seed_session_with_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Stream result", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_mermaid_output",
                        "Session chat with rendered mermaid diagrams",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "User starts session", &full);
                assertion::assert_text_in_region(frame, "Send prompt", &full);
                assertion::assert_text_in_region(frame, "Report result", &full);
                assertion::assert_text_in_region(frame, "Open diff view", &full);
                assertion::assert_text_in_region(frame, "▲", &full);
                assertion::assert_text_in_region(frame, "▼", &full);
                assertion::assert_text_in_region(frame, "CUSTOMER", &full);
                assertion::assert_text_in_region(frame, "places", &full);
                assertion::assert_text_in_region(frame, "Start new session", &full);
                assertion::assert_text_in_region(frame, "Stream result", &full);
                assertion::assert_not_visible(frame, "flowchart TD");
                assertion::assert_not_visible(frame, "erDiagram");
                assertion::assert_not_visible(frame, "sequenceDiagram");
                assertion::assert_not_visible(frame, "Agent available");
            },
        )?;

    Ok(())
}

/// Verify cyclic flowcharts in session output render as Unicode diagrams
/// instead of falling back to the raw Mermaid fenced block.
#[test]
fn session_view_cyclic_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_cyclic_mermaid_output")
        .with_terminal_size(100, 60)
        .setup(seed_session_with_cyclic_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Orchestrator controller", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "session_cyclic_mermaid_output",
                        "Cyclic Mermaid flowchart rendered in session chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Orchestrator controller", &full);
                assertion::assert_text_in_region(frame, "Typed command response", &full);
                assertion::assert_text_in_region(frame, "Session events", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Session events ───▶ Orchestrator controller",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Orchestrator controller ───▶ Agent model",
                    &full,
                );
                assertion::assert_not_visible(frame, "flowchart LR");
            },
        )?;

    Ok(())
}

/// Verify an over-wide left-to-right flowchart uses the compact top-down
/// terminal layout instead of falling back to raw Mermaid source.
#[test]
fn session_view_compact_mermaid_output() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_compact_mermaid_output")
        .with_terminal_size(100, 40)
        .setup(seed_session_with_compact_mermaid_output)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("g")
                    .wait_for_text("Qwen complete", 5000)
                    .capture_labeled(
                        "session_compact_mermaid_output_top",
                        "Top of compacted over-wide Mermaid flow",
                    )
                    .write_text("j".repeat(80))
                    .write_text("k".repeat(80))
                    .write_text("g")
                    .wait_for_text("Qwen complete", 5000)
                    .write_text("G")
                    .wait_for_text("Grafana on port 3000", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_compact_mermaid_output_bottom",
                        "Bottom of compacted over-wide Mermaid flow",
                    )
                    .write_text("q")
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                assertion::assert_not_visible(frame, "Grafana on port 3000");
                assertion::assert_text_in_region(
                    frame,
                    "Compact Mermaid output",
                    &Region::full(frame.cols(), frame.rows()),
                );
                assert_eq!(report.captures.len(), 2);
                let top_frame = common::frame_from_capture(&report.captures[0]);
                let bottom_frame = common::frame_from_capture(&report.captures[1]);
                let top_region = Region::full(top_frame.cols(), top_frame.rows());
                let bottom_region = Region::full(bottom_frame.cols(), bottom_frame.rows());

                assertion::assert_text_in_region(&top_frame, "Qwen complete", &top_region);
                assertion::assert_text_in_region(
                    &top_frame,
                    "Tracing spans and events",
                    &top_region,
                );
                assertion::assert_text_in_region(
                    &bottom_frame,
                    "Grafana on port 3000",
                    &bottom_region,
                );
                assertion::assert_text_in_region(&bottom_frame, "▼", &bottom_region);
                assertion::assert_not_visible(&top_frame, "flowchart LR");
                assertion::assert_not_visible(&bottom_frame, "flowchart LR");
            },
        )?;

    Ok(())
}
