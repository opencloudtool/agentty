//! Frontend-neutral campaign planning, reconciliation, and integration.
//!
//! Hosts provide session execution through `ag_session::SessionService`,
//! durable state through `ag_store` repositories, and notification and schedule
//! boundaries. The crate owns campaign policy and prompts without depending on
//! a terminal UI.

mod coordinator;
mod event;

pub use coordinator::{
    OrchestrationApprovalOutcome, OrchestrationCoordinator, OrchestrationSessionMetadata,
    approve_orchestration, child_session_is_stopped, controller_prompt, detach_managed_child,
    persist_controller_plan, persist_managed_child_area_compliance, running_child_count,
    session_metadata_for_project,
};
pub use event::{OrchestrationEvent, OrchestrationEventSink, OrchestrationSchedule};
