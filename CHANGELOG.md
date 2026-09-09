# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v0.15.12] - 2026-09-08

### Added

- agentty: support repository file lookup in question answers.
- ag-harness: expose model history and validate tool calls.

### Changed

- orchestration: move campaign orchestration into a reusable workspace crate.
- ag-harness: strengthen durable session persistence, recovery, and lifecycle handling.
- ag-harness: use trusted Git executables for repository tools and default the CLI to
  Git from `PATH`.
- agentty: limit concurrent subagents for native providers.
- build: limit parallelism across sessions and update Rust, dependencies, and GitHub
  Actions.
- release: bump workspace crate metadata and lockfile package versions to `0.15.12`.

### Fixed

- agentty: keep session creation, transcript rendering, and scrolling responsive.
- agentty: preserve session replay context, protocol integrity, and replacement
  continuations during resume fallback.
- agentty: skip duplicate automatic focused reviews and preserve review routing.
- agentty: track session resources without using runtime process IDs incorrectly.
- agentty: avoid optional Git locks during inspection and stop automatic commits on
  persistent index locks.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.11] - 2026-09-05

### Added

- agentty: add the `gpt-6-astra` Codex model with its large-context compaction policy.

### Changed

- ag-harness: accept bounded assistant content alongside native tool calls while
  preserving response-size limits.
- ag-harness: expand live benchmarks with persistent session reopening, lifecycle
  telemetry, filtering, rate limiting, latency reporting, and redacted provider errors.
- ag-harness: simplify file line scanning and session history budget selection.
- release: bump workspace crate metadata and lockfile package versions to `0.15.11`.

### Fixed

- agentty: restrict generated npm test scripts to owner-only executable permissions.

### Contributors

- @andagaev
- @minev-dev

## [v0.15.10] - 2026-09-03

### Added

- agentty: check for updates at startup and hourly while the application runs.
- ag-harness: persist resumable chat sessions in SQLite.
- ag-harness: provide bounded native repository tools for listing, searching, and
  reading files.

### Changed

- agentty: keep project synchronization non-modal and operation-scoped, retain session
  creation during synchronization, and expire terminal status after ten seconds.
- agentty: use direct protocol schemas for focused reviews and display review metadata
  on separate rows.
- agentty: replace Gemini 3.7 Flash with Gemini 3.8 Flash for the Gemini and Antigravity
  providers, migrating persisted selections to the replacement.
- ag-harness: replace the Muse Spark 1.2 model catalog with Muse Spark 1.3.
- ag-harness: reuse chat completion requests across retry attempts.
- deps: update Rust, workspace dependencies, and GitHub Actions.
- release: bump workspace crate metadata and lockfile package versions to `0.15.10`.

### Fixed

- agentty: suppress duplicate focused-review replies across GitHub and GitLab.
- quality: pin the feature-test version label before rendering so version-width changes
  do not invalidate every enforced GIF freshness hash.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.9] - 2026-08-31

### Added

- agentty: persist Concise, Balanced, and Detailed response styles per session and
  project default, with `/style` selection and provider-neutral turn guidance.

### Changed

- agentty: exchange focused-review findings as structured JSON across the shared
  protocol, provider prompts, and application UI.
- agentty: preserve completed file and inline Diff comments across mode changes until a
  new turn starts or the session is deleted.
- agentty: render only visible transcript rows while preserving cached wrapping and
  paragraph semantics.
- ag-harness: validate all structured model output against request schemas and content
  limits without allocating a second full-size JSON representation.
- docs: make short, conceptual documentation the repository default.
- quality: require Agentty E2E, diff-coverage, and coverage hooks before handoff.
- release: bump workspace crate metadata and lockfile package versions to `0.15.9`.

### Contributors

- @andagaev
- @minev-dev

## [v0.15.8] - 2026-08-27

### Added

- agentty: edit and submit multiline Diff comments from the review pane.
- agentty: append review-ready sessions to existing stacks and support stacks up to five
  levels deep.
- agentty: automatically address focused-review feedback in a dedicated remediation
  mode.
- agentty: create whole-file comments from selected Diff rows.
- ag-harness: support batched tool calls and provide a live provider-compatibility
  benchmark.

### Changed

- agentty: run configured pre-commit hooks before assisted rebase continuation.
- agentty: continue automatic reviews when their projects become inactive.
- agentty: protect custom session branches with remote leases during synchronization.
- ag-harness: centralize provider model configuration in a shared catalog.
- quality: require 100% diff coverage for coverable changed lines.
- deps: update `taiki-e/install-action`.
- release: bump workspace crate metadata and lockfile package versions to `0.15.8`.

### Fixed

- agentty: resume queued synchronization after a question is cancelled.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.7] - 2026-08-24

### Changed

- ag-harness-cli: publish the command-line application through crates.io while excluding
  it from `cargo-dist` archives and installers.
- deps: update `uuid` and `taiki-e/install-action`.
- release: bump workspace crate metadata and lockfile package versions to `0.15.7`.

### Contributors

- @dependabot
- @minev-dev

## [v0.15.6] - 2026-08-24

### Added

- ag-harness: export lifecycle events as OpenTelemetry GenAI spans with contract
  coverage for metric and trace payloads, hierarchy, outcomes, and shutdown behavior.
- ag-harness-cli: run interactive Muse, Kimi, and Qwen chats with bounded repository
  access, retained conversation history, and sanitized activity summaries.
- agentty: support whole-file Diff comments alongside line and range comments.

### Changed

- agentty: submit completed Diff comments with `s` from either pane.
- agentty: queue review requests during rebases and publish them after synchronization
  finalizes.
- ag-harness: move the command-line application into the dedicated `ag-harness-cli`
  crate and register live provider checks as opt-in end-to-end tests.
- agentty: remove persisted session summaries from the protocol, storage, rendering, and
  orchestration flows in favor of assistant answers and session transcripts.
- release: bump workspace crate metadata and lockfile package versions to `0.15.6`.

### Contributors

- @andagaev
- @minev-dev

## [v0.15.5] - 2026-08-22

### Added

- ag-harness: expose lifecycle observer fan-out and GenAI metrics for agent duration,
  model and tool calls, executed-tool duration, and stable turn outcomes.

### Changed

- agentty: show the active agent, model, reasoning level, and speed mode while focused
  reviews are loading.
- agentty: align changed-line focus with the selected file explorer row and review
  comment sidebar.
- ag-agent: require Antigravity CLI 1.1.18 and retain native long-context sessions
  through its persistent NDJSON transport.
- ag-harness: standardize GenAI telemetry metadata against the pinned OpenTelemetry
  semantic convention revision.
- release: bump workspace crate metadata and lockfile package versions to `0.15.5`.

### Fixed

- ag-agent: normalize Antigravity's string-valued summary fallback before strict
  protocol parsing instead of failing the turn in native schema validation.
- ag-agent: preserve sandboxed plan mode for read-only Gemini sessions while keeping
  utility prompts on standard ACP startup.
- ag-harness: harden unified-diff validation, nested response parsing, request
  preparation, and orchestration cancellation.

### Contributors

- @andagaev
- @minev-dev

## [v0.15.4] - 2026-08-20

### Changed

- agentty: support navigating and editing completed inline diff comments.
- agentty: switch session permission modes with `Shift+Tab`.
- ag-harness: run bounded repository reads and stale-safe unified-diff writes through
  the model harness.
- repo: expand Dependabot coverage to pre-commit updates, update the Rust toolchain
  weekly, and pin `prek` to `0.4.14`.
- deps: update the GitHub Actions dependency group.
- docs: clarify application-owned lifecycle telemetry.
- release: bump workspace crate metadata and lockfile package versions to `0.15.4`.

### Fixed

- agentty: prevent partial and duplicate review-comment updates.
- agentty: preserve the diff viewport and selected-range highlights while navigating
  changed lines.
- agentty: keep automatic focused reviews and diff loading recoverable across project
  switches and worktree cleanup.
- agentty: keep session synchronization responsive during branch operations.
- agentty: extend Gemini utility prompt bootstrap deadlines.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.3] - 2026-08-19

### Added

- agentty: persist per-project response-speed defaults and expose provider-compatible
  Normal and Fast session controls.

### Changed

- agentty: give Codex `Auto Edit` sessions unrestricted command access while keeping
  read-only turns restricted.
- agentty: run review assist with provider-enforced read-only permissions.
- agentty: focus selected Diff changes with `l` while keeping `Enter` for editing
  changed lines.
- agentty: render inline right-arrow math as Unicode outside code and unsupported math
  expressions.
- deps: update `taiki-e/install-action` from `2.85.11` to `2.85.13`.
- docs: document the current orchestrator architecture and phased target workflow.
- repo: remove Git Town configuration.
- release: bump workspace crate metadata and lockfile package versions to `0.15.3`.

### Fixed

- agentty: preserve running session work, queued actions, and workflow results across
  project switches.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.2] - 2026-08-18

### Added

- ag-harness: expose normalized completion metadata, token usage, stable error
  classifications, and ordered model and tool lifecycle events.
- agentty: add a `/mode` picker for persistent `Auto Edit` and fail-closed `Read Only`
  session permissions.

### Changed

- agentty: preserve queued publish and synchronization actions across project switches.
- deps: update `async-trait`, `h2`, `taiki-e/install-action`, and `thiserror`.
- docs: focus repository guidance on durable boundaries, invariants, and source-of-truth
  workflows.
- release: bump workspace crate metadata and lockfile package versions to `0.15.2`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.15.1] - 2026-08-16

### Added

- agentty: select contiguous changed-row ranges with `Shift+V` and attach inline
  comments to the selected ranges.
- ci: publish source, workspace, and end-to-end test results to Trunk when configured.

### Changed

- agentty: resolve selected review comments together with `Space` toggles and `Enter`
  submission.
- agentty: format session timers with spaces between hours, minutes, and seconds.
- ci: run end-to-end tests through a shared workflow using the digest-pinned container.
- docs: clarify the open-source code review roadmap title.
- release: bump workspace crate metadata and lockfile package versions to `0.15.1`.

### Fixed

- testty: keep stable-frame waits sensitive to terminal style and cursor redraws while
  rejecting empty startup output.

### Contributors

- @minev-dev

## [v0.15.0] - 2026-08-15

### Added

- agentty: support navigating changed lines and drafting multiple inline comments in
  Diff view, then submit completed comments together in the next turn.
- ag-harness: run policy-approved repository reads through model tools with bounded,
  symlink-safe filesystem access and provider-compatible continuation history.
- agentty: mark sessions with merge conflicts before synchronization without changing
  their indexes or worktrees.

### Changed

- agentty: load full session diffs in the background with cancelable, stale-safe
  requests for Diff view, focused reviews, and `/apply` freshness checks.
- agentty: compact uninterrupted folder chains in diff trees while preserving full
  paths, branch structure, and stable navigation.
- agentty: hide Diff actions for sessions known to have no changes and invalidate the
  cached availability after opening writable worktrees.
- agentty: keep generated review-resolution prompts out of chat and composer history
  while preserving them for transcript replay and session state.
- deps: update `taiki-e/install-action` from `2.85.9` to `2.85.10`.
- release: bump workspace crate metadata and lockfile package versions to `0.15.0`.

### Fixed

- agentty: preserve completed focused reviews across project switches until durable
  persistence settles.
- agentty: preserve active session state and worker senders when database refreshes
  fail.
- agentty: preserve shifted punctuation in direct and SSH terminals by limiting xterm
  CSI-u reporting to `tmux`.
- agentty: preserve durable session goals in generated titles while retrying transient
  provider failures and rejecting duplicated request text.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.14.7] - 2026-08-13

### Added

- agentty: unify review comments with Diff mode through Files and Comments sidebar
  focus, shared navigation, Markdown previews, comment actions, and context-sensitive
  help.

### Changed

- agentty: replace Gemini 3.6 Flash with Gemini 3.7 Flash for the Gemini and Antigravity
  providers, migrating persisted selections to the replacement.
- release: bump workspace crate metadata and lockfile package versions to `0.14.7`.

### Contributors

- @minev-dev

## [v0.14.6] - 2026-08-13

### Added

- ag-harness: add Muse structured-output support, public model identifiers, explicit
  model overrides, OTLP/HTTP metrics, and live API verification.
- ag-harness: support validated native `read` tool calls for Qwen and Kimi models.

### Changed

- agentty: show per-row added and removed totals in the diff explorer.
- agentty: add counts to populated session group headings and mute archived session
  details.
- agentty: show persisted change summaries before focused reviews.
- agentty: require `tmux` before offering worktree-opening actions.
- release: publish `ag-harness` to crates.io with the workspace release.
- agentty: remove the `Agentty Dark` color theme from the settings selector.
- persistence: extract reusable SQLite repositories and migrations into `ag-store`.
- session: centralize shared frontend-neutral session models in `ag-session`.
- ci: route VHS recording through the pinned canonical container and document pinned
  GitHub Action versions.
- deps: update the GitHub Actions and Rust dependency groups.
- release: bump workspace crate metadata and lockfile package versions to `0.14.6`.

### Fixed

- agentty: align continuation guidance with completed and canceled session actions and
  the current lowercase `c` key.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.14.5] - 2026-08-11

### Changed

- agentty: show queued work in FIFO order with calm status indicators, preserve existing
  session titles through refreshes, and generate titles in read-only mode for every
  session role.
- agentty: remove assigned GitHub issue and requested-review Inbox workflows, and skip
  automatic reviews for orchestrator controllers.
- ci: accommodate E2E rendering deadlines under CI contention and preserve valid
  feature-recording artifacts while selecting native E2E images.
- deps: update the GitHub Actions and version-update dependency groups.
- release: bump workspace crate metadata and lockfile package versions to `0.14.5`.

### Contributors

- @dependabot
- @minev-dev

## [v0.14.4] - 2026-08-09

### Added

- agentty: add provider-enforced read-only research waves with configurable
  auto-approval, separate research and implementation planning, persisted reports, and
  verification-gated follow-up work.
- ag-harness: add Kimi support, a shared OpenAI-compatible Chat Completions transport,
  and request-duration telemetry for successful, failed, and cancelled model calls.

### Changed

- agentty: queue review-request publishing behind active turns, preserve focused-review
  ordering around durable notices, and expose publish progress without breaking branch
  operation serialization.
- agentty: tighten structured session, review, and orchestration prompt contracts around
  untrusted input, workspace isolation, read-only Git access, quality checks, and
  schema-driven responses.
- agentty: reuse cached diff, Markdown, and session-row render data across layout and
  paint paths.
- ci: use pinned Rust toolchains and centralize coverage, E2E, and multi-architecture
  image validation across workflows.
- docs: add the open-source pull-request reviewer to the public roadmap.
- release: bump workspace crate metadata and lockfile package versions to `0.14.4`.

### Fixed

- agentty: clear stale terminal content when navigation changes the routed surface while
  preserving search positions and normal within-page diff rendering.
- agentty: accept only valid terminal Codex answers for completed focused reviews while
  ignoring commentary, blank fallbacks, and unsupported assistant items.

### Contributors

- @andagaev
- @minev-dev

## [v0.14.3] - 2026-08-05

### Added

- agentty: automatically verify and apply focused-review suggestions to orchestrated
  workers for up to three durable remediation passes before controller verification.

### Changed

- release: bump workspace crate metadata and lockfile package versions to `0.14.3`.

### Fixed

- agentty: route post-verification worker continuations back through focused review, and
  recover only interrupted review generation with bounded persistence backoff, while
  persisting diff-preparation failures so storage or Git failures cannot strand
  orchestrated workers.
- agentty: keep intermediate Codex commentary and blank completion fallbacks out of
  completed focused reviews, using the latest valid terminal answer instead.

### Contributors

- @minev-dev

## [v0.14.2] - 2026-08-05

### Changed

- agentty: treat orchestration touched areas as non-exclusive planning hints and use
  changed-path comparisons as verification context instead of integration gates.
- agentty: keep review-request campaign tasks active until their child sessions merge,
  complete, or report a closed review request.

### Fixed

- agentty: keep draft orchestrators cancelable after worktree creation and before their
  first goal submission.
- agentty: allow review-ready managed workers to open their worktrees through an
  explicit confirmation flow.
- git: make command execution cancellable and bounded, retry index locks without
  blocking, honor configured remotes, and preserve hook execution during squash merges.
- forge: paginate GitHub review snapshots and redact credentials embedded in remote
  URLs.
- clipboard: fall back from Wayland to X11 when initializing Linux clipboard support.
- release: bump workspace crate metadata and lockfile package versions to `0.14.2`.

### Contributors

- @minev-dev

## [v0.14.1] - 2026-08-04

### Added

- agentty: add standalone orchestration campaign boards with acceptance-criteria
  planning, read-only managed workers, question relay, verification-gated integration,
  follow-up continuations, detach ownership transfer, and approval-gated merges.

### Changed

- agentty: render embedded forge HTML in issue descriptions and review conversations
  with bounded normalization, comment filtering, entity decoding, malformed-tag
  preservation, and numeric control-character rejection.
- agentty: use the `gemini-3.1-pro-preview` identifier for Gemini 3.1 Pro across Gemini
  and Antigravity, migrate stored selections, and reject the retired identifier.
- workflow: use bounded cumulative session summaries as intent context for diff-grounded
  commit messages and pull-request descriptions.
- ci: provide checksum-verified, architecture-specific E2E tool bundles for `amd64` and
  `arm64`.
- deps: update `schemars` from `1.2.1` to `1.2.2`.
- release: bump workspace crate metadata and lockfile package versions to `0.14.1`.

### Fixed

- agentty: provide selectable answers for orchestrator clarifications and routing
  questions.
- agentty: bind orchestration question relays to their exact managed task, preserve
  controller-authored questions, prevent managed-worker `Ctrl+c` interruption, and use
  the global orchestration parallelism setting while choosing local merges or review
  requests only when verified work is ready to integrate.
- agentty: persist controller verification verdicts before integration, re-verify
  unintegrated siblings after follow-up work, and surface touched-area violations from
  managed-child diffs.
- agentty: block managed workers from review-comment turns and preserve their final
  review diff before merged worktree cleanup.
- agentty: key repeated campaign verification by generation, include the campaign goal
  in its evidence, and complete campaigns without a redundant final controller turn.
- agentty: keep session titles and group labels visible while a running orchestrator
  carries a multiline campaign-board snapshot.
- agentty: hide the unavailable `p: PR` action from orchestrator sessions.
- agentty: remove the misleading child-diff action from orchestrator campaign boards.
- agentty: route review follow-ups for settled campaign workers through their original
  branches and re-run verification after the additional work.
- agentty: align orchestrator scroll bounds with the transcript pane below the campaign
  board for line-by-line and half-page navigation.
- agentty: fail startup recovery on storage or Git errors while preserving unfinished
  operations for retry on a later launch.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.14.0] - 2026-08-03

### Added

- agentty: support orchestrated child-session workflows with validated subtask plans,
  bounded scheduling, status propagation, and session-tree navigation.
- agentty: add per-session response-speed controls and role-specific reasoning levels
  for model defaults.
- agentty: add a `grilling` skill for explicitly requested, one-question-at-a-time plan
  and decision stress tests.
- ag-harness: add a Qwen model adapter and schema-validated structured model output with
  bounded responses and diagnostics.

### Changed

- agentty: route session operations through a foreground runtime, separate orchestration
  policy from application runtime, and centralize UI surface routing and terminal input
  handling.
- agentty: use native Antigravity stream output, retire obsolete model identifiers, and
  migrate active sessions and persisted model selections to supported replacements.
- agentty: hydrate lazy transcripts from persisted history, refine provisional titles
  after actionable intent, and allow canceled sessions to continue.
- agentty: complete merged stacked reviews after manual sync and keep post-sync workflow
  statuses chronological.
- agentty: wrap slash-command selection, navigate directly to model selector rows, use
  lowercase `c` to continue done sessions, and stack over-wide Mermaid graphs
  vertically.
- agentty: harden persistence and external-command handling, validate stored lifecycle
  state, and surface terminal reader and task-shutdown failures.
- deps: update `agent-client-protocol` to `2.0.0`, `base64` to `0.23.0`, `jsonschema` to
  `0.49.1`, `tokio-util` to `0.7.19`, and pinned CI actions.
- release: bump workspace crate metadata and lockfile package versions to `0.14.0`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.7] - 2026-07-27

### Added

- agentty: add `claude-opus-5` to the Claude model picker.
- agentty: add workspace personalities, personality-aware session prompts, source
  details in slash-command results, and a code-improver agent.
- agentty: add a frontend-neutral session lifecycle API through the new `ag-session`
  crate.
- agentty: render cyclic Mermaid flowcharts in markdown previews.
- ag-harness: add the initial harness crate for backend compatibility testing.

### Changed

- agentty: preserve session diffs across refreshes and forks, resolve stacked review
  parents from their worktrees, and allow slash commands while working on those parents.
- agentty: improve prompt file mentions, highlighted file lookups, session rendering,
  and personality summary loading.
- agentty: preserve manually linked review-request metadata and handle outdated review
  threads and explicit no-change outcomes.
- ag-agent: parse Claude structured output, pass reasoning effort to Antigravity, and
  refresh npm-global Gemini CLI installations.
- ci: move E2E tooling into a verified Podman container, compile-check database queries,
  and update grouped GitHub Actions dependencies.
- docs: rename the `README.md` project title to Agentty ADE.
- deps: update `agent-client-protocol` to `1.3.0`, `async-trait` to `0.1.91`, `clap` to
  `4.6.4`, `ignore` to `0.4.31`, `serde_json` to `1.0.151`, `thiserror` to `2.0.19`,
  `time` to `0.3.54`, `tokio` to `1.53.1`, and `x11rb` to `0.14.0`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.7`.

### Fixed

- agentty: keep `Esc` from ending question turns and correct the draft continuation
  shortcut.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.6] - 2026-07-22

### Added

- agentty: batch linked review comments by marking each actionable thread to address or
  deny before submitting one session-agent turn.

### Changed

- agentty: replace Gemini 3.5 Flash and Gemini 3.1 Flash-Lite with Gemini 3.6 Flash and
  Gemini 3.5 Flash-Lite, migrating persisted selections to their replacements.
- agentty: centralize review comment selection and grouped-row presentation.
- testty: refresh the `README.md` header layout.
- ci: run E2E workflows in a pinned Linux/amd64 image and update the Zizmor action from
  `0.5.7` to `0.6.0`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.6`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.5] - 2026-07-21

### Added

- agentty: add a rendered markdown and Mermaid preview toggle for changed markdown files
  in diff view.
- agentty: start a session directly from assigned issue details, using the issue URL as
  the initial prompt.

### Changed

- agentty: group review comments by unresolved, resolved, and standalone state while
  preserving thread selection across refreshes.
- agentty: preserve prompt drafts when returning to `Sessions` and display `C` as the
  done-session continuation shortcut.
- agentty: centralize session action eligibility, prompt composer state, settings
  presentation state, and agent CLI subprocess execution behind their owning runtime
  boundaries.
- docs: refresh the `README.md` navigation, homepage workflow highlights, and session
  management showcase.
- ci: enforce coverage checks on all builds and update the grouped GitHub Actions
  dependencies.
- deps: update `uuid` from `1.23.4` to `1.24.0`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.5`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.4] - 2026-07-18

### Changed

- agentty: use `Ctrl+Z` as the shared input undo shortcut.
- agentty: keep remotely merged review sessions read-only in Active until manual target
  sync archives them and restacks any child sessions.
- deps: update `ignore` from `0.4.27` to `0.4.28`.
- ci: update `taiki-e/install-action` from `2.82.11` to `2.83.1`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.4`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.3] - 2026-07-17

### Added

- agentty: add a review-comments view for linked GitHub pull requests and GitLab merge
  requests, with selectable general comments, inline threads, current diff context, and
  read-only navigation.
- agentty: let session agents resolve selected or all actionable review comments, then
  reply to and resolve fixed GitHub and GitLab threads after a successful branch push.
- release: generate keyless Sigstore build provenance for final GitHub release artifacts
  and attach a Scorecard-discoverable bundle.

### Changed

- agentty: open the session diff with `d` from prompt chat focus and restore the
  composer draft, attachments, history, suggestions, and scroll state on return.
- agentty: persist focused review output across session and project switches.
- agentty: block local merge queueing when a forge review request is linked.
- ag-agent: route utility prompts through a typed, injectable one-shot client and
  encapsulate turn continuation state.
- ag-protocol: reorganize protocol definitions by responsibility.
- ag-clipboard: unify platform backend initialization and report invalid UTF-8 text as
  backend errors on Wayland and X11.
- ag-forge: simplify review-request adapter ownership and command workflows.
- ci: enforce 100% patch coverage for pull requests and raise the workspace line
  coverage threshold to 93%.
- release: upload the complete artifact set through a retry-safe draft before
  publication so GitHub release immutability protects every shipped file.
- docs: document GitHub release, asset, and provenance verification.
- release: bump workspace crate metadata and lockfile package versions to `0.13.3`.

### Fixed

- agentty: keep an empty prompt open when `Backspace` is pressed.

### Contributors

- @andagaev
- @minev-dev

## [v0.13.2] - 2026-07-16

### Added

- agentty: add shared semantic text editing across prompt, clarification,
  publish-branch, and launch-configuration inputs, including word and line movement,
  consistent paste behavior, and bounded undo and redo history.
- security: add a security policy covering supported versions, private vulnerability
  reporting, response and disclosure timelines, and safe harbor.
- docs: add the SonarCloud coverage badge to `README.md`.

### Changed

- agentty: bind pasted image attachments to exact placeholder occurrences and input
  revisions so editing, undo, redo, and prompt-history navigation preserve the intended
  images and clean up discarded files safely.
- agentty: align the activity heatmap with its dashboard panel by deriving panel height
  from content and removing the redundant legend and summary footer.
- agentty: render queued follow-up messages after the transcript messages and workflow
  notices that preceded them.
- agentty: normalize transcript, queued-message, and workflow-notice spacing to one
  empty line between visible messages.
- agentty: keep focused review suggestions aligned with decisions, accepted tradeoffs,
  and explanations already resolved in the session chat.
- agentty: centralize chat-focus key handling while preserving transcript scrolling,
  diff preview, prompt draft protection, and question-mode exit behavior.
- agentty: extract transcript-to-display-line assembly from `SessionOutput`, leaving the
  component responsible for layout caching, scrolling, loader effects, and painting.
- deps: update `agent-client-protocol` to `1.2.0` and `serde_with` to `3.21.0`.
- ci: run SonarQube from coverage jobs with LCOV input and remove the standalone
  security-scan workflow.
- ci: update `taiki-e/install-action` from `2.82.10` to `2.82.11`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.2`.

### Fixed

- agentty: render assigned issue rows with the active theme's text color.
- ci: grant the Pages workflow the read permissions required by its build steps.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.13.1] - 2026-07-15

### Added

- testty: add `feature::Redaction` and `FeatureDemo::redact` so callers can declare
  generated tokens the GIF freshness hash must ignore.
- testty: add `Redaction::literal` for exact-string redaction, such as the version an
  application paints in its header.
- agentty: declare the `wt/<hash>` session worktree redaction in feature tests, so a
  session's random worktree name no longer makes every recorded GIF look stale.
- agentty: redact the `Agentty v<version>` header in feature tests so release bumps do
  not stale every committed GIF hash.

### Changed

- agentty: make `TESTTY_GIF_MODE=check` and `check-only` run freshness checks for
  published feature GIFs instead of silently disabling GIF hashing, including failures
  for invalid committed hash sidecars.
- testty: distinguish missing and invalid feature GIF hash sidecars in
  `GifStatus::Stale`.
- testty: use a stable FNV-1a feature GIF frame hash so committed sidecars compare
  consistently across local machines and CI.
- testty (breaking): `feature::compute_frame_hash` now takes the caller's redaction
  rules as a second argument. Pass `&[]` to keep the previous behavior.
- agentty: nest the E2E test project and worktree directories under the test `HOME` so
  the TUI paints home-collapsed paths (`~/test-project`, `~/.agentty/wt/<hash>`) and
  feature GIF frame hashes reproduce across macOS and Linux CI.
- agentty: stub every supported agent CLI in E2E test environments — not just `claude` —
  so the default agent a new session resolves is identical on developer machines and CI.
- agentty: run feature tests and VHS recordings with color disabled so GIF hashes remain
  stable across local shells and CI.
- agentty: preserve prompt and question drafts while chat output is focused for
  scrolling, and clarify the related footer shortcuts and send labels.
- agentty: accept and queue follow-up prompts while sessions are rebasing.
- agentty: preserve review-request publishing progress across session refreshes.
- agentty: place queued synchronization notices after the active turn and before
  follow-up messages.
- ci: run coverage checks in presubmit and postsubmit workflows.
- release: bump workspace crate metadata and lockfile package versions to `0.13.1`.

### Contributors

- @andagaev
- @minev-dev

## [v0.13.0] - 2026-07-14

### Added

- agentty: add a `p` project switcher popup to the `Sessions` view that lists registered
  projects in most-recently-opened order and switches the active project in place.
- agentty: display assigned GitHub issue details, group issues by assignment, and style
  assigned issue borders.
- agentty: let `Tab` move prompt composer focus to the chat transcript for scrolling
  without losing the current draft.
- agentty: show a shared vertical scrollbar for overflowing session and `Diff` output,
  including padding that keeps wrapped content clear of the scrollbar.
- docs: add a homepage roadmap covering the Harness, Orchestrator, Assistant, and Cloud
  tracks through 2027.
- docs: add SonarCloud quality badges to `README.md`.

### Changed

- agentty: serialize running-session sync through the active worker so queued sync runs
  after the current turn and before later chat messages.
- agentty: represent summaries, reviews, workflow notices, and published-branch sync
  output with typed transient slots, explicit lifecycles, and content-keyed caches.
- agentty: keep completed transcripts and summaries visible while branch workflows
  update their transient status.
- agentty: refine focused review output with compact sections, verification-gated
  `/apply` hints, and loading, ready, and failure states that survive session refreshes.
- agentty: run manual branch and review-request publishing in the background, preserve
  durable PR and MR creation notices, and serialize publishing with auto-push work.
- agentty: enforce timeouts for forge and cleanup-critical git commands, and run merged
  session cleanup as bounded background work.
- agentty: require Clippy checks and installed pre-commit hooks in commit workflows, and
  warn without blocking when configured hooks are missing during session workflows.
- agentty: report stacked child sync failures as transient session notices.
- agentty: rename the green color theme's internal and persisted name from `hacker` to
  `green`, migrating existing settings so users keep their selected theme.
- ag-agent: apply provider-specific structured-output schema requirements, remove raw
  provider payloads from errors, and bound long CLI and transcript error details.
- ag-tui-text: bound grouped Mermaid edge generation.
- testty: make feature GIF recording opt-in, stabilize deterministic recording, batch
  PTY proof steps, and run parallel E2E feature validation in presubmit.
- docs: refresh the website and documentation experience with responsive layouts,
  accessibility improvements, static feature posters, and shared search.
- deps: bump `tachyonfx` from `0.25.0` to `0.25.1`.
- ci: bump `taiki-e/install-action` from `2.82.8` to `2.82.10`.
- release: bump workspace crate metadata and lockfile package versions to `0.13.0`.

### Removed

- agentty: remove the review-comments preview from the `Diff` view.
- agentty: remove the process-local `Logs` tab and its in-memory logging pipeline.

### Fixed

- agentty: refresh assigned issue views when observable issue state changes.
- agentty: prevent stale `InProgress` session state from starting a duplicate worker.
- agentty: treat punctuated empty focused-review suggestions as empty.
- agentty: align session-output wrapping and scroll metrics with the scrollbar gutter.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.12.9] - 2026-07-11

The `v0.12.8` release was not successful, so `v0.12.9` includes all changes originally
documented for it.

### Changed

- agentty: render more Mermaid syntax in chat — extended node shapes, arrow variants,
  invisible layout links, `&` fan-outs, flattened subgraphs, skipped styling and
  sequence control statements, truncated over-long labels, and adaptive sequence
  lifeline spacing.
- agentty: render dotted and thick Mermaid edges with inline labels.
- agentty: preserve pasted prompt indentation.
- agentty: pin the user prompt block background to a dedicated RGB surface token.
- agentty: improve session configuration and E2E stability.
- docs: document that GIF force mode requires an unsandboxed shell.

### Removed

- agentty: remove the `/qe:check` prompt slash command.

### Fixed

- agentty: support projects backed by a bare-repository worktree layout (a container
  folder holding per-branch worktrees). Previously, starting a session turn in such a
  project failed with a git status `must be run in a work tree` error.

### Contributors

- @andagaev
- @artemgoncharuk
- @dependabot
- @minev-dev

## [v0.12.8] - 2026-07-09

### Removed

- agentty: remove the `/qe:check` prompt slash command.

### Changed

- agentty: render dotted and thick Mermaid edges with inline labels.
- agentty: preserve pasted prompt indentation.
- agentty: pin the user prompt block background to a dedicated RGB surface token.
- agentty: improve session configuration and E2E stability.
- docs: document that GIF force mode requires an unsandboxed shell.

### Contributors

- @andagaev
- @minev-dev

## [v0.12.7] - 2026-07-09

### Added

- agentty: add CLI help and version options.
- agentty: support configuring maximum reasoning levels.
- agentty: add the shared `ag-tui-text` crate.

### Changed

- agentty: update the Codex model lineup and runtime configuration.
- agentty: use the session folder directly for Antigravity.
- agentty: inject the git client into startup discovery.
- agentty: wrap fenced code blocks on word boundaries.
- ag-agent: hide internal modules behind the crate root.

### Removed

- ag-xtask: remove the unused `workspace-map` command.

### Contributors

- @minev-dev

## [v0.12.6] - 2026-07-09

### Added

- ag-git: detect in-progress rebase, merge, cherry-pick, and revert operations.

### Changed

- ag-protocol: strip `$schema` metadata from transport schemas.
- deps: bump `ignore` from `0.4.26` to `0.4.27`.
- deps: bump `time` from `0.3.51` to `0.3.53`.
- Bump workspace crate metadata and lockfile package versions to `0.12.6`.

### Fixed

- agentty: block session branch pushes when a rebase, merge, cherry-pick, or revert is
  in progress, or when the worktree is not on the expected session branch.
- agentty: render Markdown in prompt blocks in session output.

### Contributors

- @dependabot
- @minev-dev

## [v0.12.5] - 2026-07-07

### Added

- agentty: add a launch configuration list editor.
- agentty: show sync conflict resolution status.
- agentty: show diff selection change counts.
- agentty: allow merging review sessions.

### Changed

- agentty: rename Open Commands to Launch Configurations across settings, docs, and
  persisted project settings.
- agentty: allow merge queueing for stacked parents.
- agentty: format review-assist output as bullets.
- ag-protocol: document the 32-character Mermaid label limit in session turn prompts.
- ci: bump `taiki-e/install-action` from `2.82.6` to `2.82.7`.
- deps: bump `agent-client-protocol` from `1.0.0` to `1.0.1`.
- Bump workspace crate metadata and lockfile package versions to `0.12.5`.

### Fixed

- agentty: render Markdown in persisted user messages in session output.
- agentty: repaint cached session output when switching themes.
- agentty: warn only on dirty `main` checkout changes.
- agentty: keep sequence-diagram previews rendering when participant or message labels
  exceed the 32-character label limit by truncating them with a trailing ellipsis, and
  draw `sequenceDiagram` self-messages as a compact lifeline loop with a visible label.
- agentty: simplify review-assist prompt verification guidance.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.12.4] - 2026-07-06

### Changed

- ag-protocol: clarify supported Mermaid output rules for session turn prompts.
- agentty: allow session sync requests to queue while session turns are running.
- agentty: include session chat history in review-assist context.
- agentty: use the shared transcript helper in review flow state.
- docs: document `TESTTY_GIF_MODE=check` for routine E2E feature validation.
- Bump workspace crate metadata and lockfile package versions to `0.12.4`.

### Contributors

- @andagaev
- @minev-dev

## [v0.12.3] - 2026-07-06

### Changed

- agentty: accept case-insensitive Mermaid diagram headers and normalize generated node
  labels.
- agentty: move agent prompt templates into the app-owned template directory.
- agentty: normalize internal import paths.
- ci: bump `taiki-e/install-action` from `2.82.5` to `2.82.6`.
- deps: bump `mockall` from `0.14.0` to `0.15.0`.
- Bump workspace crate metadata and lockfile package versions to `0.12.3`.

### Fixed

- agentty: guard review-assist prompts against import suggestions when only a diff is
  available.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.12.2] - 2026-07-05

### Changed

- ci: gate release checks on tags and rename the validation workflow.
- release: include `ag-clipboard` in crates.io publishing.
- Bump workspace crate metadata and lockfile package versions to `0.12.2`.

### Contributors

- @minev-dev

## [v0.12.1] - 2026-07-05

### Added

- ag-agent: allow Claude session turns to use `WebSearch` and `WebFetch` without
  interactive permission grants.
- ci: add end-to-end release checks.

### Changed

- ag-agent: replace session-output resume context with transcript replay.
- ag-agent: strengthen workspace isolation and review-mode prompting.
- agentty: switch session orchestration to the shared `ag-agent` APIs.
- agentty: move Agentty home resolution into the infra boundary.
- agentty: preserve review provider selection across review tasks.
- agentty: rename the Review tab to Inbox for requested-review lists.
- agentty: reorder editable session footer actions.
- agentty: render compact two-node `LR` feedback loops in session Mermaid diagrams.
- ag-protocol: prompt session-turn agents to use supported Mermaid diagrams.
- ci: replace `cargo-shear` with `cargo-machete` for unused dependency checks.
- deps: trim unused dependencies and narrow default features for workspace crates.
- docs: clarify workflow, runtime architecture, and release-check guidance.
- cargo: tune debug profiles to keep default development builds lighter while preserving
  a full-debug profile when needed.
- Bump workspace crate metadata and lockfile package versions to `0.12.1`.

### Fixed

- deps: bump `cmov` from `0.5.3` to `0.5.4` for security updates.
- agentty: fix inline markdown punctuation spacing.
- agentty: fix session worktree seeding in E2E coverage.
- agentty: use assistant answer messages for inline markdown E2E fixtures.

### Removed

- docs: remove the obsolete `testty` explicit API design spec.

### Contributors

- @andagaev
- @minev-dev

## [v0.12.0] - 2026-07-05

### Added

- agentty: render complete ```` ```mermaid ```` fenced blocks in session chat markdown
  as Unicode diagrams for simple `graph`/`flowchart`, `erDiagram`, and `sequenceDiagram`
  diagrams.

### Changed

- agentty: open settings selector dropdowns for fixed-choice values instead of cycling
  values directly with `Enter`.
- agentty: replace `arboard` clipboard image capture with the internal `ag-clipboard`
  backend for macOS, X11, and Wayland through `wl-paste`.
- agentty: require the `wl-clipboard` package for Wayland clipboard image paste.
- Bump workspace crate metadata and lockfile package versions to `0.12.0`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.11.1] - 2026-07-01

### Added

- agentty: persist default agent and model selections across sessions.
- agentty: rename the Claude Sonnet model identifier to `claude-sonnet-5` and migrate
  existing settings.

### Changed

- agentty: bound foreground event drains to keep UI event processing responsive.
- docs: add Umami analytics to the documentation site template.
- Bump workspace crate metadata and lockfile package versions to `0.11.1`.

### Contributors

- @minev-dev

## [v0.11.0] - 2026-06-28

### Added

- agentty: store raw session conversation messages in durable transcripts.
- agentty: show review request authors.
- agentty: show sync status for draft and interactive sessions.
- agentty: show project branch names in project rows.
- ci: add merge queue validation for release workflows and GitHub Actions analysis with
  zizmor.

### Changed

- agentty: centralize UI render caches and narrow post-turn workflow dependencies.
- agentty: support stacked parent sync and replies when review-ready children are
  present, including restacking children after parent merges and archiving canceled
  children by display group.
- agentty: restore direct Gemini CLI/backend support alongside Antigravity.
- agentty: prefer `.agents` skills in session commit prompts.
- docs: clarify provider CLI authentication and delegation guidance, including supported
  CLI agents.
- ci: scope workspace source tests and exclude agentty integration test targets from
  source hooks.
- Bump workspace crate metadata and lockfile package versions to `0.11.0`.

### Removed

- ag-xtask: remove repository roadmap lint and digest commands.
- skills: remove the `implementation-plan` skill and repository planning roadmap.

### Security

- ci: harden GitHub workflow security.

### Contributors

- @dependabot
- @minev-dev

## [v0.10.6] - 2026-06-21

### Added

- agentty: show project token usage metrics in the project list.

### Changed

- agentty: refresh agent CLI versions after best-effort startup updates, preserving
  provider display order and showing `updating...` while the background refresh runs.
- agentty: split session worker turn workflows into focused workflow modules.
- agentty: route prompt image paste handling through the infra client boundary.
- agentty: route session rebase assistance through the session worker.
- Bump workspace crate metadata and lockfile package versions to `0.10.6`.

### Fixed

- agentty: preserve Antigravity prose after repair failure.

### Contributors

- @minev-dev

## [v0.10.5] - 2026-06-20

### Added

- testty: `testty run <scenario.yaml>` executes a declarative YAML scenario against any
  binary, so non-Rust projects can drive TUI end-to-end tests without a Rust harness.
  The process exit code is the pass/fail signal. See
  `crates/testty/docs/scenarios-yaml.md`.
- testty: new `testty::spec` module (`ScenarioSpec`, `StepSpec`, `ExpectSpec`,
  `SpecError`, `LoweredScenario`) deserializes a scenario from YAML and lowers it onto
  the runtime engine shared with the Rust authoring API. `StepSpec`, `ExpectSpec`, and
  `SpecError` are `#[non_exhaustive]`.

### Changed

- agentty: remove deprecated direct Gemini CLI/backend support; Google-backed model
  selection now goes through Antigravity.
- Bump workspace crate metadata and lockfile package versions to `0.10.5`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

### Removed

- agentty: remove Claude Fable 5 from selectable Claude models.

## [v0.10.4] - 2026-06-13

### Added

- agentty: show installed agent CLI versions on the projects dashboard.

### Changed

- testty: publish the npm installer under the `@agentty-xyz` scope.
- Bump workspace crate metadata and lockfile package versions to `0.10.4`.

### Contributors

- @minev-dev

## [v0.10.3] - 2026-06-13

This release supersedes `v0.10.2`, whose release workflow did not complete successfully.

### Added

- agentty: add the process-local Logs tab.
- agentty: show requested review comments in the review detail view.
- agentty: manage Antigravity hidden worktree aliases.
- agentty: support CSI-u `Shift+Enter` input in tmux.

### Changed

- agentty: split project refresh from session refresh.
- agentty: cache diff layouts and reduce render-state cloning in hot render paths.
- Add regression coverage for forge and Codex helper behavior.
- Bump GitHub Actions, npm publishing, and version-update automation dependencies.
- Bump workspace crate metadata and lockfile package versions to `0.10.3`.

### Fixed

- agentty: fix Logs tab navigation and lifetime cleanup.
- agentty: fix review list section spacing.

### Contributors

- @minev-dev

## [v0.10.2] - 2026-06-12

### Added

- agentty: add the process-local Logs tab.
- agentty: show requested review comments in the review detail view.
- agentty: manage Antigravity hidden worktree aliases.
- agentty: support CSI-u `Shift+Enter` input in tmux.

### Changed

- agentty: split project refresh from session refresh.
- agentty: cache diff layouts and reduce render-state cloning in hot render paths.
- Add regression coverage for forge and Codex helper behavior.
- Bump GitHub Actions, npm publishing, and version-update automation dependencies.
- Bump workspace crate metadata and lockfile package versions to `0.10.2`.

### Fixed

- agentty: fix Logs tab navigation and lifetime cleanup.
- agentty: fix review list section spacing.

### Contributors

- @dependabot
- @minev-dev

## [v0.10.1] - 2026-06-09

### Added

- agentty: add requested review detail navigation.
- agentty: add Claude Fable 5 model support.
- testty: add `proof::junit::JunitBackend`, a `ProofBackend` that renders a
  `ProofReport` to JUnit-XML so non-Rust CIs can ingest testty proof results as test
  cases and failures.
- testty: add a proof gallery that aggregates run artifacts into an index page.
- testty: the `testty` crate now ships the language-agnostic `testty` command-line
  binary (`cargo install testty`), folding in the previously separate, never-published
  `testty-cli` crate. The command tree is in place but the verbs remain stubbed.

### Changed

- agentty: colorize reasoning labels and show session size markers as title prefixes in
  the session list.
- agentty: expand Antigravity model selection to individual Gemini model variants.
- agentty: preserve popup clearing semantics for overlay backgrounds.
- agentty: retire `gpt-5.4` Codex model selection and promote `gpt-5.5` as the default.
- agentty: route project sync through a shared orchestrator.
- agentty: use non-interactive Codex app-server approvals.
- Bump GitHub Actions and version-update automation dependencies.
- Bump workspace crate metadata and lockfile package versions to `0.10.1`.
- testty: `ProofBackend::render` now takes a single `RenderContext` argument instead of
  `(&ProofReport, &Path)`, so future render inputs can be added without breaking
  external backends. Callers using `ProofReport::save` are unaffected. (Breaking for
  custom `ProofBackend` implementations.)
- testty: `ProofError` is now `#[non_exhaustive]`. (Breaking for exhaustive matches.)
- testty: gate the CLI binary tests in the source-test hook.

### Removed

- agentty: remove the sessions list header bottom margin.
- testty: `CellStyle::from_cell` is no longer public; it leaked the `vt100::Cell`
  implementation-detail type. Use `TerminalFrame::cell_style` instead. (Breaking.)

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.10.0] - 2026-06-02

### Added

- agentty: add stacked draft session lineage and restack behavior.
- agentty: show reasoning levels next to model names in the session list.

### Changed

- agentty: beautify provider command failure output.
- agentty: select protocol schema guidance by provider capability and require temporary
  artifact cleanup in protocol prompts.
- agentty: preserve the Antigravity session worktree as the first `agy --add-dir` root.
- agentty: skip published-branch auto-push while follow-up messages are queued.
- agentty: move the runtime render throttle behind the shared `Clock` trait boundary.
- Bump GitHub Actions and version-update automation dependencies.
- Bump workspace crate metadata and lockfile package versions to `0.10.0`.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.9.6] - 2026-06-01

### Added

- agentty: show linked review request URLs in session headers.
- agentty: persist merged review hashes so continuation can keep review context.

### Changed

- agentty: reject progress-style generated session titles and tighten the title
  generation prompt.
- agentty: restrict continuation to completed sessions; canceled sessions remain
  terminal read-only.
- Refresh the README demo GIF for the current interface.
- testty: complete the explicit-import migration across docs, tests, and upgrade
  guidance.
- Bump workspace crate metadata and lockfile package versions to `0.9.6`.

### Contributors

- @andagaev
- @minev-dev

## [v0.9.5] - 2026-05-28

This release supersedes `v0.9.4`, whose release workflow did not complete successfully.

### Added

- agentty: add Antigravity CLI backend support.
- Add SonarCloud, SonarQube, cargo-audit, Dependabot GitHub Actions update checks, and
  rust-toolchain update checks to repository automation.

### Changed

- agentty: adopt Claude Opus 4.8 as the active Opus model.
- agentty: deliver Antigravity prompts with `--print`, fix print-timeout flag ordering,
  defer cancellation cleanup, support turn interruption, and sync linked review-request
  metadata after auto-push.
- agentty: disallow commit creation guidance in session-turn protocol responses.
- Migrate release and crates.io workflows to scoped OIDC credentials, add reusable
  GitHub Actions OIDC publishing workflow support for npm release jobs, and adjust
  release workflow permissions so publish escalation can complete.
- Pin shared CI `prek` installs and hook calls to explicit `prek` v0.4.3 no-build mode,
  normalize hyphenated GitHub Action inputs, and keep the security scan workflow on
  read-only contents permissions.
- Bump workspace crate metadata and lockfile package versions to `0.9.5`.

### Removed

- agentty: remove the roadmap-backed `Tasks` tab.

### Contributors

- @dependabot
- @minev-dev

## [v0.9.4] - 2026-05-28

### Added

- agentty: add Antigravity CLI backend support.
- Add SonarCloud, SonarQube, cargo-audit, Dependabot GitHub Actions update checks, and
  rust-toolchain update checks to repository automation.

### Changed

- agentty: adopt Claude Opus 4.8 as the active Opus model.
- agentty: deliver Antigravity prompts with `--print`, fix print-timeout flag ordering,
  defer cancellation cleanup, support turn interruption, and sync linked review-request
  metadata after auto-push.
- agentty: disallow commit creation guidance in session-turn protocol responses.
- Migrate release and crates.io workflows to scoped OIDC credentials and add reusable
  GitHub Actions OIDC publishing workflow support for npm release jobs.
- Pin shared CI `prek` installs and hook calls to explicit `prek` v0.4.3 no-build mode,
  normalize hyphenated GitHub Action inputs, and keep the security scan workflow on
  read-only contents permissions.
- Bump workspace crate metadata and lockfile package versions to `0.9.4`.

### Removed

- agentty: remove the roadmap-backed `Tasks` tab.
- testty: remove the `testty::prelude` wildcard re-export module. Public items are now
  reached only through their owning module paths (for example,
  `use testty::scenario::Scenario;`); see
  [`crates/testty/docs/upgrading.md`](crates/testty/docs/upgrading.md) for the migration
  note.

### Contributors

- @dependabot
- @minev-dev

## [v0.9.3] - 2026-05-20

### Added

- agentty: add Gemini 3.5 Flash as a selectable Gemini model.

### Changed

- Bump workspace crate metadata and lockfile package versions to `0.9.3`.

### Contributors

- @minev-dev

## [v0.9.2] - 2026-05-20

### Added

- agentty: add `/qe:check` as a prompt slash command that sends a checked-in
  quality-enforcement audit prompt instead of running an in-process audit engine.
- Add reusable GitHub Actions OIDC publishing workflow support for npm release jobs.
- testty: split the README into a quick-start landing page plus focused docs for
  assertions, frame diffing, journeys, proof reports, scenarios, snapshots, and
  upgrades.

### Changed

- agentty: switch Gemini prompt delivery from standard input to argument transport.
- agentty: warn on main-checkout drift and require clean preflight state before merge
  workflows continue.
- agentty: split requested reviews into personal and group sections.
- agentty: refactor session state, prompt routing, UI formatting, and database access
  into more focused modules.
- Update workspace guidance, docs, roadmap slices, and validation expectations.
- Bump workspace crate metadata and lockfile package versions to `0.9.2`.
- Bump dependency versions, including `askama` to `0.16.0` and `pulldown-cmark` to
  `0.13.4`.

### Removed

- agentty: remove the `Stats` tab and provider usage polling.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.9.1] - 2026-05-02

### Changed

- agentty: enable the `arboard` `wayland-data-control` backend so clipboard image
  capture works on Wayland compositors such as Hyprland, and update runtime-flow,
  keybinding, and workflow docs to describe X11 and Wayland clipboard sources.
- Clarify workspace crate names, validation guidance, UI helper guidance, and
  constructor guidance in agent instructions while removing obsolete nested instruction
  files from stale example/proof directories.
- Bump workspace crate metadata and lockfile package versions to `0.9.1`.

### Contributors

- @minev-dev

## [v0.9.0] - 2026-05-02

### Added

- agentty: accept image paste payloads from `Ctrl+Shift+V` and copied PNG files in the
  prompt input flow.
- agentty: add branch and path columns to the `Projects` table so repository identity is
  visible without opening a project.
- testty: add the named `StartupWait` preset enum (`Default`, `FastNative`, `SlowNode`,
  `Custom { stable_ms, timeout_ms }`) plus the `Journey::wait_for_startup_preset` and
  `Journey::wait_for_startup_default` constructors so test authors can pick a documented
  startup-wait profile instead of hand-tuning raw `(stable_ms, timeout_ms)` numbers per
  project. `StartupWait` is re-exported from `testty::prelude`. The historical
  `Journey::wait_for_startup(stable_ms, timeout_ms)` entry point keeps working and now
  routes through `StartupWait::Custom`.
- testty: add `match_*` recipe siblings for composing reusable frame assertions.

### Changed

- agentty: move the activity heatmap into the `Projects` tab, add aggregate work stats
  to the `Projects` table, keep the `Stats` tab focused on global aggregates, and hide
  non-repository workspaces from the project list.
- agentty: tighten shared tab-page spacing and split project activity from project info
  so the `Projects` tab has denser, clearer panels.
- testty: HTML proof report now renders structured `match_*` failures (those carried on
  `AssertionResult::failure`) with a side-by-side context-and-frame block. The context
  column surfaces the `Expected` variant, the optional `Region` coordinates, and the
  matched-span list; the frame column shows `AssertionFailure::frame_excerpt` inside a
  `<pre>` with a column ruler and per-row gutters whose labels are anchored to the
  region's `(col, row)` so reported coordinates match the live frame. The ruler defaults
  to the compact two-row tens-and-ones layout and adds a hundreds row above the tens row
  whenever the excerpt extends past column 99, so absolute coordinates beyond column 99
  can be recovered by stacking the digits at any tens-marked column instead of being
  truncated by `(col / 10) % 10`. The column ruler is sized by terminal cell width (via
  `unicode-width`) so wide glyphs in the excerpt do not desync the labels from real
  terminal positions. Needle highlighting in the excerpt is scoped to what the
  underlying matcher actually validated: every-match matchers (`TextInRegion`,
  `NotVisible`, `MatchCount`) wrap every occurrence of the needle in a `needle-hit` span
  using the same one-character advance as `TerminalFrame::find_text`, so overlapping
  matches like `ana` in `banana` are no longer silently dropped, while first-match-only
  matchers (`ForegroundColor`, `BackgroundColor`, `Highlighted`, `NotHighlighted`) only
  highlight the first occurrence so the report does not imply that secondary occurrences
  were checked. For every-match matchers, strictly overlapping byte ranges are merged
  into a single span so the emitted HTML stays well-formed, while adjacent matches like
  `ab` in `abab` keep their distinct spans so the report shows both hits. The
  `needle-hit` style uses background color only with no horizontal padding so
  highlighted cells stay aligned with the column ruler. The matched-span list now also
  surfaces the actual `foreground`, `background`, and style flags (`bold`, `italic`,
  `underline`, `inverse`, `dim`) carried on each `MatchedSpan`, so color- and
  highlight-style failures expose the actual cell state next to the structured
  `Expected` description instead of forcing readers back to the assertion-line summary.
  The `Expected` description for color and highlight expectations is also reworded to
  read as "first match of '<needle>' with foreground color ..." (and the equivalent for
  background, highlight, and not-highlight) so the report makes it explicit that the
  underlying matchers only validate the first matched span. Legacy entries pushed
  through `ProofReport::add_assertion` keep the historical one-line `pass`/`fail` shape
  unchanged.
- Bump workspace crate metadata and lockfile package versions to `0.9.0`.

### Fixed

- agentty: isolate E2E project discovery from the host home directory so tests only see
  repositories created for the current scenario.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.11] - 2026-05-02

### Added

- testty: add `SoftAssertions` accumulator that batches `match_*` failures and panics
  once at scope end with every recorded message. When constructed with
  `SoftAssertions::with_report`, each recorded `AssertionFailure` is also routed into
  the most recent `ProofCapture::assertions` entry through the new
  `ProofReport::record_soft_failure` plumbing so a single capture can carry every
  batched failure for the proof report. `SoftAssertions` is re-exported from
  `testty::prelude`.
- testty: re-export `AssertionResult` from `testty::prelude` so downstream code can
  inspect `ProofCapture::assertions` entries (including the structured `failure`
  payload) without naming the full module path.
- testty: add `Step::Eventually` plus the `Step::eventually` and `Scenario::eventually`
  constructors so test authors can poll any `MatchResult`-returning frame predicate
  against the live PTY frame and surface the last `AssertionFailure` on timeout instead
  of a generic timeout panic. The `FramePredicate` alias is re-exported from
  `testty::prelude`. VHS recordings approximate the new step with a fallback `Sleep` for
  the full timeout because VHS has no predicate-driven wait primitive, keeping GIF
  playback bounded by the same worst-case window the PTY executor would have observed.

### Changed

- testty: add the new `AssertionResult::failure: Option<Box<AssertionFailure>>` field so
  soft-batched failures preserve the full structured context (`Expected` variant,
  optional `Region`, matched spans, frame excerpt) for HTML and other structured proof
  backends instead of collapsing every failure into a formatted string. The annotated
  text backend renders the first line of `description` next to `[PASS]`/`[FAIL]` and
  indents continuation lines under the marker. `AssertionResult` stays a regular (not
  `#[non_exhaustive]`) struct so downstream crates can both destructure entries on
  `ProofCapture::assertions` and push their own entries with struct literals; existing
  field reads through `assertion.passed` and `assertion.description` stay
  source-compatible, but two downstream patterns break at compile time and must be
  updated together: any caller constructing `AssertionResult` directly must add the new
  `failure: None` field, and any caller exhaustively destructuring `AssertionResult`
  with a `let AssertionResult { passed, description } = ...` pattern (the destructuring
  contract pinned in `crates/testty/tests/public_api.rs`) must add the new `failure`
  binding (or a `..` rest pattern).
- testty: mark `PtySessionError` as `#[non_exhaustive]` and add the new
  `PtySessionError::Assertion(Box<AssertionFailure>)` variant so structured predicate
  failures from `Step::Eventually` flow through the existing executor return type.
  Downstream `match` arms must include a fallback `_` arm.
- agentty: replace the fixed `Step::sleep` waits in the shared E2E session-creation
  journeys (`create_session_and_return_to_list` and
  `create_session_with_prompt_and_return_to_list`) with predicate-driven
  `Step::eventually` waiters keyed off the in-session and sessions-list footer markers
  so the journeys settle as soon as the UI transitions and surface a structured
  `AssertionFailure` on timeout instead of an opaque over-sleep.
- Bump workspace crate metadata and lockfile package versions to `0.8.11`.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.10] - 2026-04-28

### Added

- agentty: add `gemini-3.1-flash-lite-preview` as a selectable Gemini model with parser,
  provider metadata, docs, and E2E model-picker coverage.
- agentty: add a project-scoped `Review` tab that lists open GitHub pull requests and
  GitLab merge requests requesting the current user's review, with refresh handling,
  navigation, help text, docs, and E2E coverage.
- testty: `PtySessionBuilder::args` forwards CLI arguments to the spawned binary so
  non-interactive subcommand flows such as `--help` and `--version` are testable through
  the existing builder pipeline.
- testty: add GIF freshness modes, hash sidecars, read-only check mode, forced
  regeneration, public API re-exports, and `TESTTY_GIF_MODE` support in the agentty
  feature-test harness.

### Changed

- agentty: start new session worktrees from the active local branch, keep published
  session rebases on the remote base ref, remove unused upstream lookup plumbing, and
  keep git command execution non-interactive.
- agentty: reuse existing open same-branch GitHub pull requests or GitLab merge requests
  for review requests, and keep requested-review cache refreshes fresher.
- agentty: route `Esc` in question mode as an end-turn shortcut when no `@`-mention
  overlay is open, while keeping overlay dismissal on `Esc`.
- agentty: preserve legacy plain-`Enter` encoding under tmux on ghostty by keeping kitty
  keyboard enhancement flags to disambiguation and alternate-key reporting.
- Bump workspace crate metadata and lockfile package versions to `0.8.10`.

### Fixed

- testty: `Frame::row_text`, `Frame::all_text`, and `Frame::text_in_region` skip
  wide-character continuation cells so wide glyphs stay contiguous with their neighbors,
  while real blank columns are preserved as spaces so substring searches cannot collapse
  text across distant columns.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.9] - 2026-04-27

### Changed

- Prefer one-shot Gemini app-server permission approvals while preserving
  policy-specific approval handling.
- Remove standalone strict lint CI steps now covered by repository-defined `prek` hooks.
- Disable `uv` caching in the shared Rust/`prek` setup to avoid stale cache
  interactions.
- Bump workspace crate metadata and lockfile package versions to `0.8.9`.

### Contributors

- @minev-dev

## [v0.8.8] - 2026-04-26

### Changed

- Install `rustfmt` in the shared Rust/`prek` setup action for both default and LLVM
  tools nightly toolchains.
- Bump workspace crate metadata and lockfile package versions to `0.8.8`.

### Contributors

- @minev-dev

## [v0.8.7] - 2026-04-26

### Changed

- Bump workspace crate metadata and lockfile package versions to `0.8.7`.
- Preserve the generated release workflow permissions needed by the crates.io publish
  job.

### Contributors

- @minev-dev

## [v0.8.6] - 2026-04-26

### Changed

- Rename the pre-start session status from `New` to `Draft` across runtime state,
  persistence, UI labels, and user documentation.
- Rename theme settings from `Current` and `Hacker` to `Agentty Default` and
  `Agentty Green`.
- Publish workspace crates through the release workflow and wait for crates.io
  publication before release announcement.
- Run strict lint checks in postsubmit, install clippy in the shared Rust/`prek` setup,
  and align CI hook usage with release checks.
- Clarify testty public API tripwire tests and session isolation documentation.

### Contributors

- @minev-dev

## [v0.8.5] - 2026-04-27

### Added

- Add a regular/draft session creation selector, plus feature-test coverage for
  draft-session cancellation and terminal-session continuation.
- Allow running sessions to be canceled from the sessions list, and allow unstarted
  draft sessions to be canceled before their worktree is created.
- Queue follow-up chat messages while a turn is running, render queued messages inline,
  and retract them one at a time with `Ctrl+C` before canceling the active turn.
- Add GitHub and GitLab review-comment previews in diff mode, background review-request
  refresh, and persisted focused-review output across restarts.
- Add `gpt-5.4-mini` as a selectable Codex model and show model-specific focused-review
  loading text.
- Add the `Dark Horizon` theme and refine theme tokens, status colors, and footer
  branch/path styling.
- Add session worktree isolation checks, main-checkout dirtiness detection, and scoped
  app-server provider approval handling.
- Add transient workflow notices and committing progress rows for auto-commit, rebase,
  and merge flows.
- Add the `bump-version` workflow skill and shared release-check setup for
  `prek`-managed CI validation.
- testty: add the curated `testty::prelude` surface, public API tripwire coverage,
  `SnapshotConfig::with_update_mode`, and Result-returning `match_*` assertion APIs.

### Changed

- Question input now uses `Ctrl+C` to end the turn without answering and `q` to return
  to the sessions list from non-text focus, while `Esc` no longer discards in-progress
  question text.
- Session worktrees and publishes now start from upstream-tracked refs so unpublished
  local base-branch commits stay out of session pull requests and merge requests.
- Auto-commit handling now treats empty amend results as no-change notices and keeps
  commit/rebase workflow notices outside the final transcript body.
- `/apply` now requires actionable focused-review suggestions before it can send an
  apply prompt.
- Open-worktree actions stay hidden while sessions are active, queued, rebasing, or
  merging.
- Documentation and release automation now use `mdformat` wrapping, a shared Rust/`prek`
  CI setup action, and release guidance in `AGENTS.md` plus `bump-version`.
- testty: prepare the crate for independent crates.io publication, document the upgrade
  path from earlier `0.x` releases, and make the public snapshot error/config surfaces
  non-exhaustive.

### Removed

- testty: remove the public `artifact`, `calibration`, and `overlay` modules, and remove
  `testty::snapshot::is_update_mode()` in favor of per-config update-mode methods.
- Remove the old `release` skill in favor of the repository release guidance and
  `bump-version` workflow.

### Fixed

- Wrap agent prompt diffs before rendering so long diff lines do not overflow the
  session output view.
- Render queued chat messages without a UI delay.
- Preserve focused-review cache and visibility across restarts, diff mode, and
  clarification flows.
- Refresh session tests and end-to-end assertions for cancellation, `/apply` guidance,
  question input, and the new feature demos.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.4] - 2026-04-25

### Added

- Queue chat messages while a turn is running and include them in session output.
- Animate in-progress session output with a Tachyonfx loader.
- Split settings into global and project sections.
- Surface FYI guidance for opening commands in the sessions list.
- Gate `/apply` to verify focused-review suggestions before applying.

### Changed

- Render done-session transcript and summary as a single stream.
- Use semantic text foreground for persisted user prompt content.
- Align summary markdown spacing with the section layout.
- Align UI component foreground style with session list text color.
- Use a stable bar glyph for spinner status indicators.
- Adopt theme-driven session status colors and broader theme styling refresh.
- Return interrupted in-progress turns to `Review`.
- Update `testty` crate description and document the feature module.

### Fixed

- Hide focused review output in terminal `Done` and `Canceled` sessions.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.3] - 2026-04-24

### Changed

- build: remove pinned version comments from release.yml

### Contributors

- @minev-dev

## [v0.8.2] - 2026-04-24

### Added

- Animate session loaders with frame-aware rendering.
- Preserve source session context when creating continuation drafts.

### Changed

- Recess clarification prompt background with dedicated palette surface.
- Route coverage and end-to-end checks to postsubmit.
- Adopt `cargo-nextest` for workspace tests and coverage.
- Pin GitHub workflows to immutable action SHAs.
- Update Context7 setup command.
- Raise pre-commit source-test coverage thresholds.

### Fixed

- Show review fallback status when no review text is available and refresh staged-draft
  sessions immediately.
- Preserve focused review visibility after completion across prompt, question, and done
  sessions.
- Preserve user-cancelled turns as canceled sessions.
- Track turn prompt source to preserve agent payload text.
- Restrict protocol questions to genuine clarifications.

### Contributors

- @andagaev
- @minev-dev

## [v0.8.1] - 2026-04-24

### Added

- Preview review-request comments inside the diff page. Press `d` from a review-ready
  session to open the diff page, then press `c` inside the page to toggle the right
  panel between the git diff and the cached comments. The comments panel lists inline
  threads and pull-request-level "General discussion" comments for GitHub-linked
  sessions; GitLab support is tracked as a follow-up.
- Add theme setting and theme-aware palette rendering.
- Add `gpt-5.5` Codex model support.
- Render session summary above active prompt during in-progress turns.
- Add session chat FYI action hints.

### Changed

- Share session-output layout caching across render and metric paths.
- Load session transcript details lazily.
- Refine roadmap linting and planning conventions.

### Fixed

- Harden pre-commit validation and fix compile errors.
- Shut down router sessions concurrently.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.8.0] - 2026-04-21

### Added

- Add terminal session continuation that seeds a new session from completed or canceled
  work.
- Add `/apply` so focused-review suggestions can be sent back to the agent as a new
  prompt.
- Add background review-request polling plus refreshed session-view guidance and demo
  coverage.

### Changed

- Refactor app orchestration, database repositories, and UI layout/state into narrower
  modules with typed `SessionId` handling.
- Improve forge workflow handling, including remote working-directory GitHub CLI
  execution and stacked review-request planning.
- Refresh release-policy, workflow, keybinding, and architecture documentation for the
  new session and review flows.

### Fixed

- Add SQLite busy-timeout handling and align WAL persistence settings for more reliable
  session storage.
- Debounce stale `@`-mention loading, clear pending session tasks correctly, and
  preserve focused-review output through clarification flows.

### Contributors

- @andagaev
- @minev-dev

## [v0.7.9] - 2026-04-18

### Changed

- Switch session worktree and branch naming from `agentty/` to `wt/` for brevity.
- Use detected session branch names in status and footer display.

### Contributors

- @minev-dev

## [v0.7.8] - 2026-04-17

### Changed

- Migrate retired `claude-opus-4-6` model IDs to `claude-opus-4-7`.
- Disable Agentty coauthor trailer by default for new projects.
- Replace `pre-commit` references with `prek` across CI and documentation.
- Unify review publish input handling in session view.
- Update `README.md` project overview and installation guidance.

### Removed

- Remove selectable `claude-opus-4-6` model (migrated to `claude-opus-4-7`).

### Contributors

- @minev-dev

## [v0.7.7] - 2026-04-16

### Added

- Add `ClaudeOpus47` model variant and set it as the default Claude model.
- Allow canceling unstarted draft sessions from the session list.
- Add render hot-path documentation for shared markdown caches.

### Changed

- Reorder session output rendering by chronological state.
- Update `setup-uv` GitHub Action to v8.1.0 in CI workflows.
- Review commit workflow guidance and align CI with pre-commit validation.

### Fixed

- Load @-mention entries from project root for unmaterialized draft sessions.
- Stabilize settings navigation E2E with seeded model settings.

### Contributors

- @andagaev
- @minev-dev

## [v0.7.6] - 2026-04-16

### Changed

- Upgrade `cargo-dist` to 0.31.0.

### Contributors

- @minev-dev

## [v0.7.5] - 2026-04-15

### Added

- Add Gemini `--acp` command invocations with cached app-session identifiers.
- Add session-output status updates for post-turn published-branch auto-pushes.

### Changed

- Delay draft-session worktree creation until the staged bundle starts so the branch is
  based on the latest local base branch.
- Split branch publishing onto `p` and forge review-request publishing onto `Shift+P`,
  with the same optional custom branch-name flow.

### Fixed

- Improve directory fuzzy matching in agent prompt path lookups.
- Hide stale focused-review output after merge and stabilize auto-review startup timing.

### Removed

- Remove the session deletion confirmation flow.

### Contributors

- @andagaev
- @minev-dev

## [v0.7.4] - 2026-04-13

### Added

- Add GitLab support to forge review requests.
- Add `feature-test` skill for E2E tests with VHS GIF generation.

### Changed

- Auto-sync already-published session branches after completed turns.
- Consolidate review-request publishing under `p`.

### Contributors

- @andagaev
- @minev-dev

## [v0.7.3] - 2026-04-08

### Added

- Render roadmap-backed Tasks tab.
- Display active model in session header.

### Changed

- Clarify Forge roadmap for GitHub Shift+P publish shortcut.

### Contributors

- @minev-dev

## [v0.7.2] - 2026-04-07

### Added

- Add review request sync action and forge indicators in session view.
- Add `Ctrl+c` stop-session shortcut in session view.
- Render inline markdown formatting in session titles.

### Changed

- Color forge indicators by review-request state in session list.
- Add `FeatureDemo` builder to testty and migrate E2E tests to `FeatureTest`.
- Add stub agent executables to e2e test environment for CI compatibility.

### Contributors

- @minev-dev

## [v0.7.1] - 2026-04-04

### Added

- Add session-scoped reasoning overrides.
- Add conditional Tasks tab for roadmap-enabled projects.
- Add interactive Mermaid controls and fit behavior to docs pages.
- Add prefilled slash composer from session view.
- Add transient `AgentReview` status for focused review generation.

### Changed

- Trigger immediate git status refresh after successful workflow outcomes.
- Synchronize staged draft titles with active prompt snapshots.
- Rename slash prompt action to commands menu.
- Improve selected session list status contrast.
- Constrain draft prompt routing to new sessions.
- Normalize @lookups for agent delivery while preserving raw prompt text.
- Refactor prompt composer into shared domain module.
- Preserve focused review when exiting diff mode and suppress rebase during
  `AgentReview`.
- Refine docs table wrapping, runtime-flow guidance, and session workflow diagrams.
- Adopt promotion-based roadmap ownership process.

### Fixed

- Fix legacy Codex usage migration (`gpt-5.3-codex` to `gpt-5.4`).
- Handle Gemini ACP usage parsing for additional quota payload formats.

### Dependencies

- Bump `agent-client-protocol` from 0.10.3 to 0.10.4.

### Contributors

- @dependabot[bot]
- @minev-dev

## [v0.7.0] - 2026-04-02

### Added

- Add diff preview from question mode with state snapshot restoration.
- Add protocol-repair retry for malformed agent responses.
- Add session lifecycle E2E tests and shared git/session helpers.
- Add quality check guidance to protocol instructions.
- Add VHS feature GIF generation from E2E tests with content-hash caching.

### Changed

- Keep completed-turn metadata above active streaming prompt.
- Batch reducer handling of repeated `AgentResponseReceived` events.
- Refine branch push authentication guidance and cancel summary handling.
- Cache turn applied state in reducer batch.
- Modularize app startup, review, and persistence helpers.
- Refactor app-server runtimes into focused modules.
- Apply reducer projection for completed turns.
- Queue VHS feature GIF generation and Zola auto-discovery roadmap slices.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.12] - 2026-04-01

### Added

- Add draft session workflow state and staged draft metadata handling.
- Add session diff stats plus base and remote git status tracking in the UI.
- Add broader unit and E2E coverage, including modular `testty`-backed session,
  navigation, confirmation, and showcase flows.
- Add a features page with per-feature GIF demos to the docs site.

### Changed

- Persist and reuse app-server instruction bootstrap state across restored sessions.
- Scope model selection and auto-commit defaults to locally runnable backends and
  project setting names.
- Propagate typed error enums through app, session, app-server transport, and CLI
  boundaries.
- Restrict follow-up tasks to code changes and default new follow-up prompts to the
  first available task.
- Restructure E2E coverage into multi-module tests and refresh roadmap planning
  guidance.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.11] - 2026-03-28

### Added

- Launch and reopen sibling sessions from follow-up tasks.
- Show session active-work timers and metadata in UI lists.
- Add `testty` E2E proof pipeline with native rendering and frame diffing.
- Add roadmap queue tooling and planning workflows.

### Changed

- Migrate E2E tests to the `testty` proof pipeline and remove the `check-indexes` hook.
- Propagate typed errors through app, session, and infra layers.
- Keep unchanged review sessions in view mode.
- Remove GitLab review request support.
- Streamline prompt suggestion handling and question-mode interactions.
- Replace directory indexes with semantic agent guidance.

### Contributors

- @andagaev
- @dependabot
- @minev-dev

## [v0.6.10] - 2026-03-26

### Added

- Add a docs-site blog section.
- Add roadmap metadata requirements for step headings, IDs, assignees, and claim
  commits.

### Changed

- Rename the Rust-native TUI E2E crate from `ag-tui-test` to `testty` and prepare it for
  crates.io publishing.
- Remove the manual review-request sync flow and keep `end_turn_no_answer` aligned with
  `Review`.
- Refocus implementation-roadmap guidance on active follow-up work and simpler step
  operations.
- Remove `#[ignore]` gates from E2E tests.
- Clarify session commit prompt guidance to consult repository commit conventions.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.9] - 2026-03-25

### Added

- Add `s` keybinding to sync review request status in session view.
- Add `testty` TUI E2E testing framework with PTY-driven semantic assertions.

### Changed

- Migrate git infrastructure module from `Result<..., String>` to typed `GitError`.
- Tolerate extra fields in protocol deserialization while keeping schema strict.
- Improve protocol parse diagnostics.
- Separate summary transcript from streamed output.
- Strip markdown fences in protocol parser.
- Dim question panel when chat input is focused.

### Fixed

- Fix wrapped plain-text utility output test assertion after diagnostics refactor.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.8] - 2026-03-19

### Added

- Add a scrollbar to long diffs.

### Changed

- Change Esc in question mode to end the turn instead of skipping one question.
- Clarify read-only git command guidance in agent prompts.
- Refine Rust-native TUI E2E framework plan.

### Fixed

- Recover trailing protocol payload from wrapped provider output.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.7] - 2026-03-19

### Added

- Add session follow-up task implementation plan.

### Changed

- Require strict protocol JSON for Codex, utility, and all agent responses.
- Prefer structured Gemini completion payload and accept wire-type defaults.
- Track published session branch git statuses across refreshes.
- Support keyboard enhancement flags in terminal runtime.
- Optimize session activity and refresh queries.
- Limit home-directory project scans to startup.
- Clarify that agents do not create commits automatically.

### Contributors

- @minev-dev

## [v0.6.6] - 2026-03-18

### Added

- Add VHS-based E2E testing framework with screenshot comparison.

### Changed

- Refresh session workflow and settings docs.
- Keep tracked upstream branches current in the footer.
- Keep prompt file index unbounded within max depth.
- Keep publish branch shortcut keys in the input field.
- Extract shared input key utilities and add emacs-style editing to question input.
- Document shared-host test thread budget.

### Refactored

- Introduce typed `DbError` for database operations.

### Fixed

- Confirm review session cancellation.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.5] - 2026-03-17

### Added

- Add `AGENTS.md` files for app-server, CLI, and shared modules.
- Add the tech debt error handling implementation plan.

### Changed

- Publish session branches with `git push --force-with-lease`.
- Update agent test expectations for generic agent wording and the current commit
  message model.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.4] - 2026-03-16

### Refactored

- Refactor provider routing and shared app-server helpers.
- Stream Claude responses with live schema-validated events.

### Contributors

- @minev-dev

## [v0.6.3] - 2026-03-16

### Added

- Add tech-debt and security-audit analysis skills.
- Add at-mention file completion to question mode.
- Pass `--effort` flag to Claude CLI based on reasoning level.
- Generate protocol profiles with self-descriptive schemas.

### Changed

- Refactor agent prompt preparation and split protocol subsystem.
- Move app-server clients under agent backends.
- Append change summaries and preserve summary payloads.
- Format done session summary as markdown sections.
- Parameterize runtime over generic `Backend` for in-process TUI testing.
- Restrict `a` (new session) shortcut to the Sessions tab.
- Finish typed SQLx query mapping cleanup.
- Refine implementation plans for meta-agent skills and execution backends.

### Fixed

- Keep prompt mention state in sync with cursor.
- Fix footer bar branch rendering.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.2] - 2026-03-12

### Added

- Add multi-installer auto-update implementation plans.

### Changed

- Make filesystem reads asynchronous.
- Unify protocol instruction prompt templates.
- Refactor publish branch input mode handling.
- Document setting keys and simplify publish updates.
- Refine session commit coauthor trailer handling.
- Expand session database and app regression test coverage.
- Handle startup app initialization errors gracefully.

### Fixed

- Fix test and rebase mock failures on main.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.1] - 2026-03-12

### Added

- Add background auto-update with status bar progress and `--no-update` flag.
- Add codecov badge to README.
- Add postsubmit coverage workflow.
- Add chat output scrolling to question-answer mode.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.0] - 2026-03-11

### Added

- Support pasted prompt images across all session backends.
- Add auto-update implementation plan.

### Changed

- Session prompt footer uses shared help styling.
- Simplify auto-update plan to background npm install with status bar progress.
- Add test failure protocol to quality gates in AGENTS.md.
- Update test prompt assignments to use `.into()` conversion.
- Plan docs omit rendered size sections.
- Stream Claude and Gemini prompts through stdin.
- Surface Claude auth guidance for command failures.
- Harden prompt image workflow lifecycle.

### Contributors

- @andagaev
- @minev-dev

## [v0.5.11] - 2026-03-11

### Changed

- Track prompt image attachments in prompt mode.
- Add size budgets to implementation plans.
- Clarify implementation-plan AGENTS purpose.
- Rename implementation plan priorities to steps.
- Standardize titled substeps in implementation plans.
- Consolidate architecture guide map into landing page.
- Document single evolving session commit flow.

### Contributors

- @minev-dev

## [v0.5.10] - 2026-03-10

### Changed

- Refine detached session rollout plan.
- Branch push adds forge review request links.
- Refine prompt image paste plan.

### Contributors

- @minev-dev

## [v0.5.9] - 2026-03-10

### Changed

- Align implementation plans with updated skill rules.
- Scope prompt image paste plan to session chat composer.
- Generate session commit messages from cumulative diffs.
- Prefer shallower file index matches.

### Contributors

- @minev-dev

## [v0.5.8] - 2026-03-10

### Fixed

- **Git:** Ignore HTTPS userinfo in remote parsing.

### Contributors

- @minev-dev

## [v0.5.7] - 2026-03-10

### Added

- **Plan:** Add session commit message flow plan.
- **UI:** Add branch publish popup for custom remote targets.
- **Session:** Persist published upstream refs for sessions.
- **Architecture:** Extract forge review-request code into `ag-forge`.

### Changed

- **UI:** Replace review request flow with manual branch publish.
- **Projects:** Skip stale project directories in project list.
- **Docs:** Replace docs plan symlinks with explicit indexes and format plan headings.

### Contributors

- @minev-dev

## [v0.5.6] - 2026-03-09

### Added

- **Agent:** Add predefined answer options for agent questions.
- **Agent:** Add mandatory per-turn change summaries to agent protocol.
- **Infra:** Implement forge CLI review-request adapters (GitHub/GitLab PR/MR
  workflows).
- **UI:** Add session view review request workflows (create, open, refresh).
- **UI:** Show project scope in list tabs.
- **Settings:** Scope settings per project.
- **Review:** Auto-start focused review generation on session Review transition and
  cache results.

### Changed

- **UI:** Change focused review navigation to open/regenerate with exit key.
- **UI:** Remove external editor shortcut (`e`).
- **Git:** Remove session worktrees when review sessions are canceled.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.5.5] - 2026-03-07

### Added

- **Skill:** Add `implementation-plan` skill for managing project plans.
- **Docs:** Add Forge review request support plan.
- **Docs:** Add GitHub issue form templates and directory indexing.
- **Infra:** Add default pull request template for the repository.
- **UI:** Add Agentty info panel to the project list.
- **UI:** Chat panel gets polished chrome and unified overlay styling with dimmed
  backdrop.

### Changed

- **Codex:** Promote `gpt-5.4` as the default Codex model.
- **UI:** Keep question input visible in tight terminal layouts.
- **Docs:** Refine Agentty description.

### Fixed

- **Skill:** Clarify requirements and plan structure for `implementation-plan` skill.

### Contributors

- @minev-dev

## [v0.5.4] - 2026-03-06

### Added

- **Docs:** Define plan template and add test coverage improvement plan.
- **UI:** Add background tints for changed lines in diff view.

### Changed

- **UI:** Tab bar gains separators and muted border styling.

### Contributors

- @minev-dev

## [v0.5.3] - 2026-03-06

### Added

- **UI:** Support `Alt+Enter` and `Shift+Enter` newline entry across settings and
  prompt.
- **Session:** Generate session titles from user intent when the first start turn
  begins.

### Changed

- **UI:** Migrate UI color usage to semantic palette tokens.
- **UI:** Refresh table visual styling across list and stats pages.
- **UI:** Render clarification prompts with distinct spacing and styling.
- **UI:** Render footer help as styled keybinding lines.
- **UI:** Session list uses shared page margin.
- **UI:** Stop `@mention` highlighting before trailing punctuation.
- **Session Output:** Improve verbatim markdown wrapping and Unicode width handling.
- **Session Output:** Preserve multiline user prompts across persistence and rendering.
- **Review:** Tighten review suggestion severity criteria and require concise actionable
  suggestions.
- **Architecture:** Refactor module roots into router-only modules.
- **Claude:** Enforce strict MCP config for Claude backend.
- **Docs:** Reframe UI beautification plan around implementation status.

### Fixed

- **Session Output:** Prevent duplicate final assistant output after streaming.

### Contributors

- @minev-dev

## [v0.5.2] - 2026-03-05

### Added

- **UI:** Support multiline open command editing in the settings tab.
- **UI:** Render session status header directly above the output panel.
- **Session:** Add open command selector when multiple launch commands are configured
  for a worktree.
- **Docs:** Add `CLAUDE` and `GEMINI` symlinks for module-level `AGENTS.md` files.

### Changed

- **Architecture:** Rename `app` and `ui` module roots to singular names (`app.rs`,
  `ui.rs`, `domain.rs`, `infra.rs`, `runtime.rs`).
- **Architecture:** Parse merge commit messages from structured protocol output.
- **Architecture:** Harden `AGENTS.md` index validation and normalize directory index
  links.
- **Docs:** Prevent content and table-of-contents text overflow on the documentation
  site.
- **Docs:** Expand runtime flow documentation and add doc comments across migration,
  runtime, UI, and database helpers.

### Contributors

- @minev-dev

## [v0.5.1] - 2026-03-04

### Added

- **UI:** Show active session count in the project list.
- **Review:** Run focused review assist in isolated start mode.
- **Docs:** Split architecture documentation into a dedicated section and document
  structured response protocol.

### Changed

- **Protocol:** Harden structured protocol handling across providers.
- **Architecture:** Standardize module-oriented imports across the `app` and `ui`
  layers.
- **Architecture:** Align architecture docs with runtime mode, channel schema, and test
  boundaries.
- **Quality:** Require explicit user approval to retain legacy behavior during
  development.

### Fixed

- **UI:** Fix active session count calculation to exclude `Question` status and ensure
  projects reload on session refresh.

### Contributors

- @minev-dev
- @andagaev

## [v0.5.0] - 2026-03-04

### Added

- **UI:** Handle agent clarification questions in a dedicated question mode with
  persistent history.
- **UI:** Highlight `@mention` tokens in chat input.
- **UI:** Improve chat input wrapping and viewport scrolling.
- **UI:** Align session output wrapping with panel borders.
- **Architecture:** Switch agent output to schema-validated JSON messages and normalize
  assist protocol output.
- **Architecture:** Inject `FsClient` and `Clock` dependencies into session workflows
  for better testability.
- **Architecture:** Route session stats and filesystem workflows through the app layer.
- **Docs:** Add diff-first verification guidance and refine site responsiveness.

### Changed

- **Session:** Move first-turn title generation to the start-turn worker and use plain
  text.
- **Session:** Filter Codex thought/reasoning text from persisted assistant output and
  handle it separately during streaming.
- **Session:** Remove plan messages from the agent response protocol.
- **Session:** Prefer active sessions for initial selection in the UI.
- **Protocol:** Prefer agent message content over trailing reasoning payloads.

### Removed

- **UI:** Remove session stop shortcut (`Ctrl+c`) and stop-session flow.
- **Architecture:** Remove unused `nix` dependency.

### Fixed

- **Session:** Ensure merge cleanup (worktree/branch removal) completes before marking
  session as `Done`.
- **Sync:** Optimize session output synchronization for append-heavy updates.
- **Review:** Parse structured agent responses in focused review assist correctly.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.7] - 2026-03-04

### Added

- **UI:** Add `Ctrl+u` prompt-line deletion and intent-aware confirmations for merge,
  delete, and quit actions.
- **Settings:** Persist Codex reasoning levels and propagate reasoning identifiers
  through runtime integrations.
- **Environment:** Allow `AGENTTY_ROOT` to override the default agentty data root.

### Changed

- **UI:** Remove the session list project column and highlight the active project name
  in the `Sessions` tab.
- **Session:** Preserve live session state during reload and unify child PID propagation
  across assist/rebase/merge flows.
- **Docs:** Split usage docs into dedicated workflow and keybindings pages and refine
  rebase conflict guidance.
- **Quality:** Require full validation checks during execution and standardize the full
  test command to single-threaded runs.

### Fixed

- **Tests:** Refresh stale assertions after settings/help text updates.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.6] - 2026-03-02

### Added

- **UI:** Add structured question-answer flow between agents and users.
- **UI:** Add stable paragraph anchor links and dual sidebars in docs.
- **UI:** Colorize added/removed line counts in diff page title.
- **UI:** Project list highlights active project.
- **Docs:** Add design and architecture documentation page.
- **Docs:** Add GitHub metadata badges and right-side anchor links.
- **Docs:** Add tooling setup instructions for `uv` and `pre-commit`.
- **Session:** Generate session titles once in background and refine generation
  instructions.
- **Session:** Persist and resume provider-native conversation identifiers across
  restarts.
- **Architecture:** Add structured agent response protocol with metadata delimiter.
- **Architecture:** Unify session turn execution through agent channels.

### Changed

- **Codex:** Load usage limits lazily and remove usage panel/polling.
- **Codex:** Update model defaults and remove `gpt-5.2-codex`.
- **UI:** Refine info overlay and session list padding.
- **Architecture:** Extract process boundaries (tmux, editor, sync) and inject
  dependencies (clock, sleeper, git) for better testability.
- **Architecture:** Propagate assistant phase metadata in app-server streams.
- **Sync:** Render sync success details as markdown sections.

### Fixed

- **Session:** Harden runtime shutdown I/O and handle Claude partial streaming.
- **UI:** Harden diff selection fallback and simplify sync commit title formatting.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.5] - 2026-03-01

### Added

- **UI:** Add Cmd+Backspace current-line deletion in prompt.
- **Settings:** Add default review model and separate default smart/fast model settings.
- **Review:** Enforce read-only constraints for focused review assist and refine prompt
  structure.

### Changed

- **UI:** Show diff line-change totals in diff panel title.
- **UI:** Center loading sync text in info overlay and show only OK action.
- **Sync:** Improve sync completion details and info overlay presentation; show newly
  pulled commit titles.
- **Settings:** Rename DevServer setting to OpenCommand.
- **Session:** Session manager replays review history after restart.
- **Tokens:** Read turn usage from thread token usage updates.
- **Codex:** Adjust auto-compaction threshold by model.
- **Docs:** Style docs tables with borders, hover states, and responsive scrolling.

### Removed

- **Backend:** Remove env-based backend selection (`AGENTTY_AGENT`).
- **Startup:** Remove lock module from runtime startup.

### Fixed

- **Review:** Prevent runtime leaks and simplify focused review handling.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.4] - 2026-02-28

### Added

- **UI:** Add focused review mode to session view and diff mode.
- **Docs:** Add MCP docs section, Context7 setup guide, and workflow documentation.
- **Codex:** Enable live web search and network access.
- **Docs:** Redirect docs landing page to getting started overview.

### Changed

- **UI:** Keep info overlay action row visible for multiline messages.
- **UI:** Handle shifted J/K scrolling in diff mode and block actions for canceled
  sessions.
- **Sync:** Improve sync popup guidance for push authentication failures and render sync
  success metrics on separate lines.
- **Git:** Set non-interactive git prompt defaults for repo commands.
- **Architecture:** Backend command construction uses one build API.

### Contributors

- @minev-dev

## [v0.4.3] - 2026-02-26

### Added

- **ACP:** Integrate typed Gemini ACP protocol transport and tests.
- **ACP:** Send empty Gemini ACP client capabilities on initialize.
- **Session:** Use Askama templates for session prompts and propagate render errors.

### Changed

- **Gemini:** Remove reconnect banner and rename Gemini stdout reader.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.2] - 2026-02-26

### Added

- **Codex:** Add auto-compact support for Codex app-server sessions.

### Contributors

- @minev-dev

## [v0.4.1] - 2026-02-26

### Added

- **UI:** Add `Shift+Arrow` and `Alt/Shift+Backspace` word-wise cursor movement in
  prompt mode.
- **Models:** Stream Gemini assistant chunks to the UI during turns and handle ACP
  permission requests.
- **Skills:** Add code review skill.
- **Tests:** Add regression tests and improve coverage with mocked clients.

### Changed

- **UI:** Project switcher supports `j`/`k` navigation, unfiltered list navigation, and
  visible selection.
- **UI:** Report sync outcome details in completion popup and keep confirmation choices
  visible.
- **UX:** List mode opens canceled sessions on `Enter`.
- **Models:** Codex resume uses `--last` only without replay history and enforces high
  reasoning effort.
- **Models:** Rename Gemini Pro preview variant to Gemini 3.1.
- **Session Output:** Keep clean auto-commit silent, report no-op states, and ignore
  synthetic Codex completion messages.
- **Architecture:** Centralize git command execution and isolate Codex usage-limit
  loading.
- **Docs:** Update documentation to highlight Agentty self-hosting and align docs page
  widths.

### Fixed

- **Models:** Handle Gemini `session/new` error responses explicitly and ignore empty
  assistant chunks.
- **UI:** Stabilize site header across routes.

### Removed

- **UI:** Remove quick project switcher mode and overlay.

### Contributors

- @andagaev
- @minev-dev

## [v0.4.0] - 2026-02-24

### Added

- **Projects:** Add a projects tab with quick project switching.
- **Navigation:** Add backward tab navigation with `Shift+Tab`.
- **Docs:** Add the getting started overview guide.

### Changed

- **App:** Resolve main repository roots via git and exclude session worktrees.
- **UI:** Switch to `Sessions` after project selection and compact footer help actions.
- **Runtime:** Route app-server turns by provider, include root `AGENTS.md`
  instructions, and pass session folder/model in Codex payloads.
- **Docs:** Reorganize site sections, standardize skill headers, and migrate the docs
  site to compiled Sass styling.
- **Models:** Add support for the `gpt-5.3-codex-spark` model.

### Fixed

- **Database:** Fix SQLite migration `025` to avoid non-constant defaults.
- **Templates:** Fix malformed Tera block syntax in the base template.
- **Docs:** Remove duplicate front matter delimiters in overview content.

### Removed

- **Onboarding:** Remove the onboarding page from the list-mode flow.
- **Projects:** Remove project favorite controls from the project list.

### Contributors

- @andagaev
- @minev-dev

## [v0.3.0] - 2026-02-23

### Added

- **Docs:** Add copy button to code blocks.
- **Docs:** Add theme selector and favicon to site.
- **Docs:** Add contributing guide and templates.
- **Claude:** Enable Bash tool for Claude agent.
- **Output:** Stream Codex turn events to session output.

### Changed

- **Sync:** Fix pull rebase to target explicit upstream.
- **UI:** Cap chat input panel height and scroll prompt viewport.
- **Architecture:** Generalize app-server session handling.
- **Architecture:** Refactor site templates to use base layout.
- **UI:** Update docs sidebar styling.
- **Project:** Update repository URLs to new organization.
- **Architecture:** Move UI rendering into a dedicated render module.
- **Architecture:** Extract shared stdio JSON-RPC transport utilities.
- **UI:** Adopt Builder Lite pattern for UI components.
- **Project:** Update description to "Agentic Development Environment (ADE)".
- **Runtime:** Track active turn usage from completion and stream events.
- **UX:** Align view mode shortcuts with session state rules.
- **Runtime:** Require strict turn ID matching and make prompt char handling sync.
- **Output:** Filter synthetic completion status lines from chat output.
- **Deps:** Bump pulldown-cmark from 0.13.0 to 0.13.1.

### Removed

- **Command Palette:** Remove command palette and multi-project switching.
- **Docs:** Remove documentation sections and demo assets from README.
- **Slash Commands:** Remove `/clear` slash command and session history clearing logic.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.2.2] - 2026-02-22

### Added

- **Release:** Add crates.io publish workflow for release tags.

### Changed

- **Metadata:** Add full workspace author metadata.

### Contributors

- @minev-dev

## [v0.2.1] - 2026-02-22

### Added

- **Session Output:** Add toggle to switch between summary and full output for completed
  sessions.
- **Release:** Require explicit confirmation for version bump type in release skill.
- **Runtime:** Track active turn ID to prevent race conditions during turn completion.

### Changed

- **Architecture:** Refactor UI routing and overlays into dedicated modules and
  centralize frame drawing.
- **Session:** Defer session cleanup and load at-mention entries asynchronously for
  faster startup.
- **Git:** Retry git commands on index lock contention and simplify session view
  handling.
- **Settings:** Only persist default model when the "last-used" option is enabled.
- **Rebase:** Improve recovery from stale metadata during rebase assist.
- **Permissions:** Consolidate permission handling into a single "Auto Edit" mode.

### Removed

- **Permissions:** Remove legacy permission mode column from database and UI.
- **Permissions:** Remove non-auto permission modes and plan follow-up functionality.

### Contributors

- @andagaev
- @minev-dev

## [v0.2.0] - 2026-02-22

### Added

- **Plan:** Add iterative plan question flow with per-question answer options.
- **Sync:** Run branch sync in background with loading popup and outcome display.
- **Sync:** Add session branch sync action with sync-blocked popup.
- **Sync:** Add assisted conflict resolution for sync main rebase.
- **Stats:** Add Codex usage limits to stats dashboard.
- **Stats:** Persist session-creation activity and render by local day.
- **Stats:** Persist and display all-time model usage and longest session duration.
- **Help:** Help system uses state-aware action projection.
- **Dev Server:** Add editable Dev Server setting and run when opening session tmux
  window.
- **UX:** Add `h`/`l` shortcuts for confirmation selection.

### Changed

- **Architecture:** Refactor agent infrastructure into provider modules.
- **Architecture:** Split git infrastructure and UI utilities into focused modules.
- **Architecture:** Inject `GitClient` into app workflows and isolate multi-command git
  tests.
- **Refactor:** Move file indexing into infra module and parse using `pulldown-cmark`.
- **Refactor:** Rename state, file, and mode modules for clarity.
- **Refactor:** Move module roots from `mod.rs` to sibling files.
- **Sync:** Add project and branch context to sync popups.
- **Sync:** Sync main branch by pushing after rebase.
- **Plan:** Improve plan follow-ups and Codex stats limit rendering.
- **UX:** Use shared confirmation mode for quit and session deletion.
- **UX:** Confirmation prompts default to "No" selection.
- **UX:** Hide open-worktree shortcut for done sessions and restrict view actions while
  running.
- **Commit:** Preserve a single evolving session commit.
- **Search:** Prioritize basename matches in file list fuzzy scoring.

### Fixed

- **Codex:** Fix app-server error status recovery and wait for responses before parsing
  limits.
- **Stability:** Fix launch and lint regressions after rebase.
- **UI:** Deduplicate list background rendering and reset grouped session table offset.

### Removed

- **Refactor:** Remove orphaned top-level source files from `src/`.
- **Refactor:** Remove `pr-testing` directory references.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.14] - 2026-02-21

### Added

- **Stats:** Add activity heatmap to the Stats tab.
- **Stats:** Track per-model session usage and render usage summaries.
- **Settings:** Add settings tab and persist default model.
- **Diff View:** Split diff view into file list and content panels with file explorer
  navigation.
- **Diff View:** Render changed files as a tree in the file explorer.
- **Diff View:** Filter diff view content by selected file explorer item.
- **Site:** Add agentty.xyz documentation site with GitHub Pages deployment workflow.

### Changed

- **Architecture:** Refactor codebase into domain, infrastructure, and UI state modules.
- **Architecture:** Move tab state into a dedicated tab manager.
- **Session List:** Group sessions by merge queue and separate archived sessions with
  placeholders.
- **Session List:** Align session navigation with grouped list order.
- **Session Output:** Render session output and user prompt blocks as markdown.
- **Session Output:** Preserve multiline user prompt block spacing and verbatim
  rendering.
- **Merge Queue:** Queue session merges in FIFO order and handle queued sessions across
  app and UI.
- **Merge Queue:** Advance merge queue progression and retry on git index lock failures.
- **Merge:** Treat already-applied squash merges as successful.
- **Rebase:** Harden rebase assist loop against partially resolved conflicts.
- **Output:** Task service batches streamed output before flushing.
- **Output:** Separate streamed response messages for Codex output spacing.
- **Models:** Load default session model from persisted setting.
- **Models:** Use npm semver for version checks and restore version display in status
  bar.
- **Prompt:** Handle multiline paste and control-key newlines in prompt input.
- **Site:** Redesign landing page with dark terminal theme, Tailwind CSS v4, and theme
  selector.
- **Deps:** Bump dependency versions.

### Fixed

- **Build:** Fix refactor regressions and restore build stability after module
  restructure.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.1.13] - 2026-02-19

### Added

- **Session Output:** Render styled markdown in session chat output.
- **Session Output:** Switch to stream-json output and parse Gemini stream events.
- **Session Output:** Extract session output into dedicated UI component.
- **Update Check:** Show update availability in status bar and onboarding page.
- **Models:** Update Gemini Pro to version 3.1 and Claude Sonnet to version 4.6.
- **Models:** Add verbose flag to Claude stream-json commands.

### Changed

- **Session Metadata:** Move session status to output panel title and metadata to chat
  input border.
- **Session Titles:** Persist session title and summary from squash commit message.
- **Session Titles:** Use full prompt as session title for new sessions.
- **Session Replay:** Replay session transcript once after model switch.
- **Git Actions:** Remove session commit count and always show git actions.
- **Diff View:** Use merge-base for session diff to accurately exclude base branch
  updates.
- **Rebase:** Refactor rebase logic into a reusable workflow.
- **Database:** Make session token stats non-nullable with zero defaults.
- **NPM:** Update package name to `agentty` in docs and badges.

### Fixed

- **UI:** Fix session list table column layout constraints.
- **Runtime:** Add shutdown signal to event reader thread for cleaner exit.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.12] - 2026-02-19

### Added

- **Session UX:** Added a delete confirmation mode with selectable actions for session
  deletion.
- **Output Streaming:** Added a live single-line progress indicator in chat and spacing
  before the first streamed response chunk.
- **Agent Runtime:** Added Codex output streaming during non-interactive runs and
  follow-up actions for plan mode replies.

### Changed

- **Git Runtime:** Completed async `git` module transition to `spawn_blocking` and
  updated call sites.
- **Session Model:** Refactored sessions to derive `AgentKind` from `AgentModel`,
  removed the session `agent` column, and migrated legacy PR statuses to `Review`.
- **Merge/Rebase:** Improved merge and rebase robustness by auto-committing pending
  changes before merge/rebase and broadening auto-commit assistance handling.
- **UI:** Improved session list layout with minimum-width columns and title truncation,
  and added spacing around user input in session chat output.
- **Automation:** Split pre-commit workflow into separate autofix and validation phases.
- **Config:** Removed `npm-scope` from `dist-workspace.toml`.

### Removed

- **Pull Requests:** Removed pull request functionality.
- **UI Cleanup:** Removed delete confirmation bottom hints.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.11] - 2026-02-16

### Added

- **Permissions:** Add per-session permission mode toggle and `Plan` permission mode
  with denial-gated response parsing.
- **Session Control:** Add `Ctrl+c` to stop running agent sessions.
- **Prompt History:** Implement prompt history navigation with up/down arrows.
- **Stats Page:** Add project and model columns to the stats page.
- **Session Size:** Compute session size from diff and display it in the session list.
- **File Listing:** Include directories in `@` mention dropdown with trailing slash.
- **Session Status:** Add `Rebasing` status to session lifecycle.
- **Terminal Summaries:** Persist terminal summaries for session outcomes.

### Changed

- **Architecture:** Refactor app into manager composition with event-driven session
  state updates and reducer-based routing for git status, PR control, and session
  mutations.
- **Architecture:** Split session module and centralize lookups; separate session
  snapshots from runtime handles.
- **Session Defaults:** New sessions inherit the latest session's agent, model, and
  permission mode.
- **File Listing:** Include non-ignored dotfiles in file listing.
- **Merge Flow:** Run session merges asynchronously, harden merge messaging, and
  increase merge commit message timeout.
- **Rebase Flow:** Improve assisted rebase continuation flow and auto-commit pending
  changes before rebasing.
- **Auto-Commit:** Improve auto-commit recovery with agent assistance.
- **Session Summary:** Backfill and use session summary for finished sessions.
- **UI:** Move open worktree keybinding to chat view and update session size color
  palette.
- **Docs:** Document app module architecture and public API docs; add cargo install
  instructions to README.

### Removed

- **Health Module:** Remove health check module and wiring.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.10] - 2026-02-15

### Added

- **Review Workflow:** Added an explicit `Merging` session status and a review-session
  rebase action.
- **Session UX:** Added read-only controls for done sessions and a `/clear` slash
  command.
- **Help UI:** Added a `?` keybinding with an updated overlay and descriptive
  slash-command menu.

### Changed

- **Session List:** Split session metadata into `Project`, `Model`, and `Status` columns
  with dynamic width sizing.
- **Runtime:** Run session commands through per-session workers and restore interrupted
  sessions into `Review`.
- **Stats:** Accumulate token usage over time and preserve stats after `/clear`.
- **Merge Flow:** Enforce merge commit message formatting and normalize co-author
  trailer handling.
- **UI Cleanup:** Removed agent labels from session list rows and session chat titles.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.1.9] - 2026-02-13

### Added

- **Diff View:** Added diff content wrapping to render long changed lines without
  truncation.
- **Diff View:** Added structured parsing with line-number gutters (`old│new`) for
  unified diffs.
- **Docs:** Added a demo GIF to the README and documented GIF generation with VHS.

### Changed

- **Diff View:** Compare against each session's base branch so review shows all
  accumulated changes.
- **Workflow:** Simplified commit flow by auto-committing after agent iterations and
  removing manual commit mode.
- **Release Docs:** Added contributor-list requirements and examples to the release
  workflow documentation.

### Contributors

- @minev-dev

## [v0.1.8] - 2026-02-13

### Added

- **Onboarding:** Added a full-screen onboarding page shown when no sessions exist.
- **Tests:** Added onboarding behavior coverage for app state, list mode `Enter`
  handling, and UI rendering conditions.

### Changed

- **UX:** Pressing `Enter` from the onboarding view now creates a new session and opens
  prompt mode directly.
- **Error Handling:** Session creation errors in list mode are now surfaced instead of
  being silently ignored.
- **UI:** Kept the footer visible during onboarding and simplified session list
  rendering to consistently use the table layout.

### Contributors

- @minev-dev

## [v0.1.7] - 2026-02-12

### Added

- **UI:** Show session worktree path and branch in the footer bar for better context
  awareness.
- **UI:** Display commit count in the session chat title.
- **Stats:** Add session token usage statistics to the Stats page.

### Changed

- **Persistence:** Moved application data directory from `/var/tmp/.agentty` to
  `~/.agentty` for better persistence and standard compliance.
- **UX:** Renamed "Roadmap" tab to "Stats" to better reflect its content.
- **UX:** Use shortened 8-character UUIDs for session folders and git branches to reduce
  clutter.
- **Internal:** Standardized session ID variable naming across the codebase.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.6] - 2026-02-10

### Added

- **Session Status:** Added a `Committing` status to make commit progress explicit in
  the session lifecycle.

### Changed

- **Persistence:** Persist session prompt/output history in SQLite and load it on
  startup so chat history survives app reloads.
- **Session Output:** Parse agent JSON output and display only the response message in
  session output.
- **GitHub Integration:** Parse GitHub PR responses using typed serde structs and move
  GitHub CLI logic into a dedicated `gh` module.
- **PR Workflow:** Treat closed pull requests as canceled sessions and show a loader
  while PR creation is in flight.
- **Commit Flow:** Improve asynchronous session commit handling and remove placeholder
  commit output in view mode.
- **Documentation:** Extract git commit guidance into the shared skills documentation.

### Fixed

- **Tests:** Stabilized merge cleanup testing to avoid environment-dependent blocking
  during release verification.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.5] - 2026-02-08

### Added

- **Tests:** Added runtime mode handler coverage tests.
- **Documentation:** Added local `AGENTS.md` files and enforced folder index checks.
- **Documentation:** Added Context7-first rule for retrieving latest tool info.
- **Documentation:** Documented dependency injection testability guidance.

### Changed

- **Architecture:** Modularized app and runtime into focused modules (`app/` and
  `runtime/`).
- **Runtime:** Injected event source into the runtime event loop for better testability.
- **Session:** Made agent and model configurations session-scoped.
- **Linting:** Refined clippy lint configuration, tightening policies and re-enabling
  pedantic rules.
- **Skills:** Symlinked the entire skills directory for agents and refactored release
  skill.
- **Refactor:** Refactored long handlers to enforce clippy line limits.

### Contributors

- @minev-dev

## [v0.1.4] - 2026-02-08

### Added

- **Session Identity:** Migrated session IDs to UUIDs for stable identification.
- **Session Management:** Added a forward-only migration system for schema changes.
- **UI:** Added nullable title support to sessions.
- **UI:** Improved chat input with indentation preservation on wrapped lines.

### Changed

- **Session Ordering:** Sessions are now strictly ordered by `updated_at` (latest
  first).
- **Performance:** Implemented incremental session state refresh to reduce database
  load.
- **UX:** Moved prompt cursor by visual wrapped lines for better navigation.
- **Internal:** Use `String` directly for session IDs in `AppMode` and command flows.
- **Internal:** Refactored health checks into flat pass/fail checks.
- **Database:** Manage session timestamps directly in SQLite.
- **Database:** Use multiline SQL strings for better query readability.

### Removed

- Removed project-filtered session loader.
- Removed git worktree suffix from initial session prompt.
- Removed Reply mode; unified into session chat page.

### Contributors

- @minev-dev

## [v0.1.3] - 2026-02-08

### Added

- **Backends:** Added Codex backend support.
- **Project Management:** Added project switching with automatic sibling discovery.
- **Diff View:** Show all file changes in diff view.
- **Status:** Show status as text in session list and chat title.
- **Health:** Added version normalization for agent checks.

### Changed

- **Concurrency:** Converted event loop to async to fix TUI freezing on macOS.
- **Input:** Improved multiline input editing.
- **Workflow:** Enforced review-based session status transitions.
- **Performance:** Reduced tick rate to 50ms for smoother output.
- **Locking:** Replaced `fs2` with `std` file locking.
- **Formatting:** Added code formatting rules and applied to `ag-xtask`.

### Fixed

- Fixed UI freezing on macOS during agent execution.
- Clarified git worktree requirements in README.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.2] - 2026-02-08

### Added

- **GitHub Integration:** Added 'p' command to create GitHub Pull Requests (draft by
  default).
- **GitHub Integration:** Added GitHub CLI health check with nested auth sub-check.
- **UI:** Centralized icons into a reusable `Icon` enum.
- **UI:** Improve command palette with arrow navigation and auto-select.
- **Database:** Persist session status to the database.

### Changed

- **UX:** Use `/` selector in command palette dropdowns.
- **UX:** Ensure exactly one blank line before the spinner in chat view.
- **Health:** Rename Claude health check label to Claude Code.
- **Internal:** Refactor PR creation logic and tests.
- **Internal:** Optimize quality gates for AI agents.

### Removed

- Remove custom Gemini configuration creation.

### Contributors

- @minev-dev

## [v0.1.1] - 2026-02-08

### Added

- **Database:** Introduce SQLite via SQLx for session metadata.
- **UI:** Add command palette with agents selection.
- **UI:** Add health check splash screen via `/health` command.
- **UI:** Add git status indicator to footer bar.
- **Docs:** Add installation guide to README.

### Changed

- **Async:** Convert sync DB wrapper and thread spawns to native async.
- **Tooling:** Replace `cargo-machete` with `cargo-shear` in quality gates.
- **UI:** Use tilde for home directory in footer.
- **Internal:** Reorder struct fields by visibility and name.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.0] - 2026-02-08

- Initial release.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev
