# Agentty Source

Nested guides specialize the application, domain, infrastructure, runtime, and UI
boundaries.

## Invariants

- Persisted `setting` and `project_setting` keys come from `ag_session::SettingName`,
  re-exported by `domain/setting.rs`; do not add ad hoc keys or aliases.
- `ag_session::SessionStatus::can_transition_to()` is the lifecycle source of truth. Do
  not restate or implement a second transition graph in Agentty.
