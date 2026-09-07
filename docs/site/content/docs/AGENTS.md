# Documentation Content

Keep this subtree focused on current user behavior and durable architecture. Do not use
it as a changelog, implementation inventory, or agent-environment setup guide.

## Writing

- Keep overview pages concise and task-oriented; link to the detailed workflow,
  keybinding, backend, or architecture page instead of repeating it.
- Preserve useful headings and anchors. Use short titled sections for prose and tables
  only for compact comparison data.
- Use checked Zola `@/...` links for internal Markdown targets; do not use relative
  `.md` or browser-relative links.
- Keep the clean-machine path executable: document Git and installation plus
  authentication of at least one supported agent CLI before first launch.
- Keep provider prerequisites, authentication, invocation surfaces, and account caveats
  aligned among `README.md`, `getting-started/installation.md`, and
  `agents/backends.md`. `README.md` is the concise public source of truth.
- Keep protocol and transport internals out of user pages unless users need them to
  operate Agentty.

## Sources of Truth

- Runtime mode handlers, visible tab labels, and
  `crates/agentty/src/presentation/help_action.rs` define shortcut names and
  availability. Document both directions of tab navigation.

## Change Routing

- Use `getting-started/overview.md` only for concepts and first-run flow.
- Update `agents/backends.md` for visible backend or model behavior.
- Update `usage/workflow.md` and `usage/keybindings.md` for UI flow and controls.
- Update `architecture/module-map.md` for ownership, `architecture/runtime-flow.md` for
  orchestration or visibility, and `architecture/testability-boundaries.md` for external
  traits.
- Keep forge documentation aligned with every supported forge family and CLI.
- When a product surface is removed, remove its page and navigation entry rather than
  leaving a historical stub.

Before handoff, scan edited pages for duplicated behavior, stale setup, long table
cells, invalid internal links, and implementation detail at the wrong layer.
