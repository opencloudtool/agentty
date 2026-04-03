//! App-event batch reduction helpers.

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;

use super::branch_publish::BranchPublishActionUpdate;
use super::core::AppEvent;
use super::review::ReviewUpdate;
use crate::app::session_state::SessionGitStatus;
use crate::app::{UpdateStatus, session};
use crate::domain::agent::AgentModel;
use crate::domain::session::SessionSize;
use crate::infra::agent::AgentResponse;
use crate::infra::file_index::FileEntry;

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
pub(crate) struct AppEventBatch {
    /// Latest structured agent responses keyed by session id.
    pub(crate) agent_responses: HashMap<String, AgentResponse>,
    /// Latest loaded at-mention entries keyed by session id.
    pub(crate) at_mention_entries_updates: HashMap<String, Vec<FileEntry>>,
    /// Most recent branch-publish action result in the batch.
    pub(crate) branch_publish_action_update: Option<BranchPublishActionUpdate>,
    /// Whether a fresh git status snapshot is present.
    pub(crate) has_git_status_update: bool,
    /// Whether a fresh latest-version snapshot is present.
    pub(crate) has_latest_available_version_update: bool,
    /// Latest project ahead/behind status.
    pub(crate) git_status_update: Option<(u32, u32)>,
    /// Latest available stable version when reported.
    pub(crate) latest_available_version_update: Option<String>,
    /// Review-assist updates keyed by session id.
    pub(crate) review_updates: HashMap<String, ReviewUpdate>,
    /// Session ids whose runtime handles changed.
    pub(crate) session_ids: HashSet<String>,
    /// Latest session git-status snapshots keyed by session id.
    pub(crate) session_git_status_updates: HashMap<String, SessionGitStatus>,
    /// Persisted session model changes keyed by session id.
    pub(crate) session_model_updates: HashMap<String, AgentModel>,
    /// In-progress thinking text keyed by session id.
    pub(crate) session_progress_updates: HashMap<String, Option<String>>,
    /// Recomputed diff stats keyed by session id.
    pub(crate) session_size_updates: HashMap<String, (u64, u64, SessionSize)>,
    /// Whether the batch requested a full session reload.
    pub(crate) should_force_reload: bool,
    /// Latest sync-main completion result in the batch.
    pub(crate) sync_main_result:
        Option<Result<session::SyncMainOutcome, session::SyncSessionStartError>>,
    /// Latest auto-update status in the batch.
    pub(crate) update_status: Option<UpdateStatus>,
}

/// Reducer utilities for draining and coalescing queued app events.
pub(crate) struct AppEventReducer;

impl AppEventReducer {
    /// Drains the current app-event queue into one ordered vector.
    pub(crate) fn drain(
        event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
        first_event: AppEvent,
    ) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }

        events
    }

    /// Reduces raw app events into one coalesced batch for the tick.
    pub(crate) fn reduce(events: Vec<AppEvent>) -> AppEventBatch {
        let mut event_batch = AppEventBatch::default();
        for event in events {
            event_batch.collect_event(event);
        }

        event_batch
    }
}

impl AppEventBatch {
    /// Collects one app event into the coalesced batch state.
    pub(crate) fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                self.at_mention_entries_updates.insert(session_id, entries);
            }
            AppEvent::GitStatusUpdated {
                session_statuses,
                status,
            } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
                self.session_git_status_updates = session_statuses;
            }
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => {
                self.has_latest_available_version_update = true;
                self.latest_available_version_update = latest_available_version;
            }
            AppEvent::UpdateStatusChanged { update_status } => {
                self.update_status = Some(update_status);
            }
            AppEvent::SessionModelUpdated {
                session_id,
                session_model,
            } => {
                self.session_model_updates.insert(session_id, session_model);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SyncMainCompleted { result } => {
                self.sync_main_result = Some(result);
            }
            AppEvent::SessionSizeUpdated {
                added_lines,
                deleted_lines,
                session_id,
                session_size,
            } => {
                self.session_size_updates
                    .insert(session_id, (added_lines, deleted_lines, session_size));
            }
            AppEvent::BranchPublishActionCompleted {
                restore_view,
                result,
                session_id,
            } => {
                self.branch_publish_action_update = Some(BranchPublishActionUpdate {
                    restore_view,
                    result: *result,
                    session_id,
                });
            }
            AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            } => {
                self.review_updates.insert(
                    session_id,
                    ReviewUpdate {
                        diff_hash,
                        result: Ok(review_text),
                    },
                );
            }
            AppEvent::ReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            } => {
                self.review_updates.insert(
                    session_id,
                    ReviewUpdate {
                        diff_hash,
                        result: Err(error),
                    },
                );
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
            AppEvent::AgentResponseReceived {
                response,
                session_id,
            } => {
                self.agent_responses.insert(session_id, response);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::branch_publish::{BranchPublishTaskResult, BranchPublishTaskSuccess};
    use crate::domain::agent::AgentModel;
    use crate::domain::session::SessionSize;
    use crate::infra::agent::AgentResponse;
    use crate::ui::state::app_mode::{ConfirmationViewMode, DoneSessionOutputMode};

    /// Builds a restore-view snapshot used by branch-publish reducer tests.
    fn confirmation_view_mode_fixture(session_id: &str) -> ConfirmationViewMode {
        ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: session_id.to_string(),
        }
    }

    /// Builds a successful branch-publish reducer payload for one session.
    fn pushed_branch_result(branch_name: &str) -> BranchPublishTaskResult {
        Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: branch_name.to_string(),
            review_request_creation_url: None,
            upstream_reference: format!("origin/{branch_name}"),
        })
    }

    /// Verifies reducer coalescing keeps the latest value for repeated
    /// same-session updates in one tick.
    #[test]
    fn reduce_keeps_last_same_session_values() {
        // Arrange
        let events = vec![
            AppEvent::SessionModelUpdated {
                session_id: "session-a".to_string(),
                session_model: AgentModel::Gemini3FlashPreview,
            },
            AppEvent::SessionModelUpdated {
                session_id: "session-a".to_string(),
                session_model: AgentModel::Gemini31ProPreview,
            },
            AppEvent::SessionProgressUpdated {
                progress_message: Some("first".to_string()),
                session_id: "session-a".to_string(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: Some("second".to_string()),
                session_id: "session-a".to_string(),
            },
            AppEvent::SessionSizeUpdated {
                added_lines: 1,
                deleted_lines: 2,
                session_id: "session-a".to_string(),
                session_size: SessionSize::S,
            },
            AppEvent::SessionSizeUpdated {
                added_lines: 8,
                deleted_lines: 13,
                session_id: "session-a".to_string(),
                session_size: SessionSize::L,
            },
            AppEvent::SessionUpdated {
                session_id: "session-a".to_string(),
            },
            AppEvent::SessionUpdated {
                session_id: "session-a".to_string(),
            },
            AppEvent::AgentResponseReceived {
                response: AgentResponse::plain("first answer"),
                session_id: "session-a".to_string(),
            },
            AppEvent::AgentResponseReceived {
                response: AgentResponse::plain("second answer"),
                session_id: "session-a".to_string(),
            },
        ];

        // Act
        let batch = AppEventReducer::reduce(events);

        // Assert
        assert_eq!(
            batch.session_model_updates.get("session-a"),
            Some(&AgentModel::Gemini31ProPreview)
        );
        assert_eq!(
            batch.session_progress_updates.get("session-a"),
            Some(&Some("second".to_string()))
        );
        assert_eq!(
            batch.session_size_updates.get("session-a"),
            Some(&(8, 13, SessionSize::L))
        );
        assert_eq!(batch.session_ids.len(), 1);
        assert_eq!(
            batch
                .agent_responses
                .get("session-a")
                .map(AgentResponse::to_answer_display_text)
                .as_deref(),
            Some("second answer")
        );
    }

    /// Verifies review and branch-publish coalescing uses final-wins
    /// semantics for the same tick.
    #[test]
    fn reduce_uses_final_wins_for_review_and_branch_publish_events() {
        // Arrange
        let events = vec![
            AppEvent::ReviewPrepared {
                diff_hash: 11,
                review_text: "first review".to_string(),
                session_id: "session-a".to_string(),
            },
            AppEvent::ReviewPreparationFailed {
                diff_hash: 12,
                error: "latest failure".to_string(),
                session_id: "session-a".to_string(),
            },
            AppEvent::ReviewPrepared {
                diff_hash: 21,
                review_text: "stable review".to_string(),
                session_id: "session-b".to_string(),
            },
            AppEvent::BranchPublishActionCompleted {
                restore_view: confirmation_view_mode_fixture("session-a"),
                result: Box::new(pushed_branch_result("feature/first")),
                session_id: "session-a".to_string(),
            },
            AppEvent::BranchPublishActionCompleted {
                restore_view: confirmation_view_mode_fixture("session-b"),
                result: Box::new(pushed_branch_result("feature/final")),
                session_id: "session-b".to_string(),
            },
        ];

        // Act
        let batch = AppEventReducer::reduce(events);

        // Assert
        assert_eq!(
            batch.review_updates.get("session-a"),
            Some(&ReviewUpdate {
                diff_hash: 12,
                result: Err("latest failure".to_string()),
            })
        );
        assert_eq!(
            batch.review_updates.get("session-b"),
            Some(&ReviewUpdate {
                diff_hash: 21,
                result: Ok("stable review".to_string()),
            })
        );
        assert_eq!(
            batch.branch_publish_action_update,
            Some(BranchPublishActionUpdate {
                restore_view: confirmation_view_mode_fixture("session-b"),
                result: pushed_branch_result("feature/final"),
                session_id: "session-b".to_string(),
            })
        );
    }
}
