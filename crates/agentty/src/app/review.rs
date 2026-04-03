//! Focused review-cache and review-assist orchestration helpers.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::core::AppEvent;
use super::task;
use crate::domain::agent::AgentModel;
use crate::domain::session::{Session, Status};
use crate::infra::git::GitClient;
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, HelpContext};

/// Cached focused review state for a session.
#[derive(Debug)]
pub(crate) enum ReviewCacheEntry {
    /// Review generation is in progress.
    Loading {
        /// Hash of the diff text that triggered this review generation.
        diff_hash: u64,
    },
    /// Review text was successfully generated.
    Ready {
        /// Hash of the diff text that was reviewed.
        diff_hash: u64,
        /// Generated review text.
        text: String,
    },
    /// Review generation failed with an error description.
    Failed {
        /// Hash of the diff text that triggered the failed review.
        diff_hash: u64,
        /// Human-readable error description.
        error: String,
    },
}

impl ReviewCacheEntry {
    /// Returns the diff content hash stored in any variant.
    pub(crate) fn diff_hash(&self) -> u64 {
        match self {
            Self::Loading { diff_hash }
            | Self::Ready { diff_hash, .. }
            | Self::Failed { diff_hash, .. } => *diff_hash,
        }
    }

    /// Builds one cache entry from a completed focused-review result.
    pub(crate) fn from_result(diff_hash: u64, result: &Result<String, String>) -> Self {
        match result {
            Ok(review_text) => Self::Ready {
                diff_hash,
                text: review_text.clone(),
            },
            Err(error) => Self::Failed {
                diff_hash,
                error: error.clone(),
            },
        }
    }
}

/// Aggregated review assist output keyed by session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewUpdate {
    /// Hash of the diff that triggered this review, carried from the task.
    pub(crate) diff_hash: u64,
    /// Completed review assist result for the matching session.
    pub(crate) result: Result<String, String>,
}

/// Mutable render-state target for one focused-review-capable mode.
struct ReviewModeTarget<'a> {
    /// Status banner shown while focused review loads or fails.
    review_status_message: &'a mut Option<String>,
    /// Generated focused review text shown in the active mode.
    review_text: &'a mut Option<String>,
}

/// Computes a deterministic hash of diff text for cache invalidation.
///
/// Uses [`DefaultHasher`] which is not guaranteed to produce stable hashes
/// across Rust versions. This is acceptable because the cache is purely
/// in-memory and lives only for the duration of the process.
pub(crate) fn diff_content_hash(diff: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    diff.hash(&mut hasher);

    hasher.finish()
}

/// Spawns one focused review-assist task for the provided session diff.
pub(crate) fn start_review_assist(
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    review_model: AgentModel,
    session_id: &str,
    session_folder: &Path,
    diff_hash: u64,
    review_diff: &str,
    session_summary: Option<&str>,
) {
    task::TaskService::spawn_review_assist_task(task::ReviewAssistTaskInput {
        app_event_tx,
        diff_hash,
        review_diff: review_diff.to_string(),
        session_folder: session_folder.to_path_buf(),
        session_id: session_id.to_string(),
        review_model,
        session_summary: session_summary.map(str::to_string),
    });
}

/// Applies review assist updates for all sessions in one reducer batch.
pub(crate) fn apply_review_updates(
    review_cache: &mut HashMap<String, ReviewCacheEntry>,
    mode: &mut AppMode,
    review_updates: HashMap<String, ReviewUpdate>,
) {
    for (session_id, review_update) in review_updates {
        apply_review_update(review_cache, mode, &session_id, review_update);
    }
}

/// Starts focused review generation for sessions that just entered review.
///
/// Sessions returning to `InProgress` clear their cached review immediately so
/// the next completed diff triggers a fresh assist run.
pub(crate) async fn auto_start_reviews(
    review_cache: &mut HashMap<String, ReviewCacheEntry>,
    session_ids: &HashSet<String>,
    previous_session_states: &HashMap<String, Status>,
    sessions: &[Session],
    git_client: Arc<dyn GitClient>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    review_model: AgentModel,
) {
    for session_id in session_ids {
        let Some(session) = sessions.iter().find(|session| session.id == *session_id) else {
            continue;
        };

        let current_status = session.status;
        let previous_status = previous_session_states.get(session_id).copied();

        if current_status == Status::InProgress {
            review_cache.remove(session_id);

            continue;
        }

        let transitioned_to_review =
            current_status == Status::Review && matches!(previous_status, Some(Status::InProgress));

        if !transitioned_to_review {
            continue;
        }

        let session_folder = session.folder.clone();
        let base_branch = session.base_branch.clone();
        let session_summary = session.summary.clone();

        let diff = git_client
            .diff(session_folder.clone(), base_branch)
            .await
            .unwrap_or_default();

        if diff.trim().is_empty() || diff.starts_with("Failed to run git diff:") {
            continue;
        }

        let new_hash = diff_content_hash(&diff);
        let existing_hash = review_cache.get(session_id).and_then(|entry| match entry {
            ReviewCacheEntry::Ready { .. } => Some(entry.diff_hash()),
            _ => None,
        });

        if existing_hash == Some(new_hash) {
            continue;
        }

        review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading {
                diff_hash: new_hash,
            },
        );
        start_review_assist(
            app_event_tx.clone(),
            review_model,
            session_id,
            &session_folder,
            new_hash,
            &diff,
            session_summary.as_deref(),
        );
    }
}

/// Applies one review assist update to cache and active render state.
fn apply_review_update(
    review_cache: &mut HashMap<String, ReviewCacheEntry>,
    mode: &mut AppMode,
    session_id: &str,
    review_update: ReviewUpdate,
) {
    let ReviewUpdate { diff_hash, result } = review_update;
    let Some(cache_entry) = review_cache.get(session_id) else {
        return;
    };

    if cache_entry.diff_hash() != diff_hash {
        return;
    }

    review_cache.insert(
        session_id.to_string(),
        ReviewCacheEntry::from_result(diff_hash, &result),
    );

    if let Some(mode_target) = review_mode_target(mode, session_id) {
        apply_review_result(
            mode_target.review_status_message,
            mode_target.review_text,
            result,
        );
    }
}

/// Returns the focused-review render fields for the active mode.
fn review_mode_target<'a>(mode: &'a mut AppMode, session_id: &str) -> Option<ReviewModeTarget<'a>> {
    match mode {
        AppMode::View {
            review_status_message,
            review_text,
            session_id: view_session_id,
            ..
        } if view_session_id == session_id => Some(ReviewModeTarget {
            review_status_message,
            review_text,
        }),
        AppMode::Help {
            context:
                HelpContext::View {
                    review_status_message,
                    review_text,
                    session_id: view_session_id,
                    ..
                },
            ..
        } if view_session_id == session_id => Some(ReviewModeTarget {
            review_status_message,
            review_text,
        }),
        AppMode::OpenCommandSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. }
        | AppMode::ViewInfoPopup { restore_view, .. } => {
            confirmation_review_mode_target(restore_view, session_id)
        }
        AppMode::List
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::Diff { .. }
        | AppMode::Help { .. }
        | AppMode::View { .. } => None,
    }
}

/// Returns focused-review fields stored in one confirmation restore view.
fn confirmation_review_mode_target<'a>(
    restore_view: &'a mut ConfirmationViewMode,
    session_id: &str,
) -> Option<ReviewModeTarget<'a>> {
    if restore_view.session_id != session_id {
        return None;
    }

    Some(ReviewModeTarget {
        review_status_message: &mut restore_view.review_status_message,
        review_text: &mut restore_view.review_text,
    })
}

/// Applies one review assist result to render-state fields.
fn apply_review_result(
    review_status_message: &mut Option<String>,
    review_text: &mut Option<String>,
    result: Result<String, String>,
) {
    match result {
        Ok(text) => {
            *review_status_message = None;
            *review_text = Some(text);
        }
        Err(error) => {
            *review_status_message = Some(format!("Review assist unavailable: {}", error.trim()));
            *review_text = None;
        }
    }
}
