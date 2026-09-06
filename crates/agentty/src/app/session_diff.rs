//! Background full-diff request tracking and reducer-owned completion handling.

use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::path::PathBuf;

use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::warn;

use crate::app::review::{self, FocusedReviewPersistence, ReviewAgent, ReviewCacheEntry};
use crate::app::task::{SessionDiffTaskInput, SessionDiffTaskSource, TaskService};
use crate::app::{App, session};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{Session, SessionId, SessionRole, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::db::DbError;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget, DiffSidebarFocus,
};

/// Maximum number of delayed persistence attempts after an automatic-review
/// deferral write fails.
const MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES: u8 = 3;

/// One delayed automatic-review deferral persistence attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeferredAutoReviewPersistenceRetry {
    /// One-based delayed retry number.
    pub(crate) attempt: u8,
    /// Session whose automatic focused review remains deferred.
    pub(crate) session_id: SessionId,
}

impl DeferredAutoReviewPersistenceRetry {
    /// Wraps one initial write before any delayed retries have run.
    fn initial(session_id: SessionId) -> Self {
        Self {
            attempt: 0,
            session_id,
        }
    }

    /// Returns the next bounded retry, or `None` after the retry limit.
    fn next(self) -> Option<Self> {
        (self.attempt < MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES).then(|| Self {
            attempt: self.attempt.saturating_add(1),
            session_id: self.session_id,
        })
    }
}

/// Completed session-diff task ready for stale-safe reducer application.
pub(crate) struct SessionDiffUpdate {
    /// Request generation assigned when the background task started.
    pub(crate) request_id: u64,
    /// Full diff text or the normalized load failure.
    pub(crate) result: Result<String, String>,
    /// Session whose pending continuation owns this result.
    pub(crate) session_id: SessionId,
}

/// Foreground continuation waiting on one background diff result.
pub(crate) struct PendingSessionDiffRequest {
    _cancellation: DropGuard,
    purpose: SessionDiffPurpose,
    session_id: SessionId,
}

/// Stable focused-review inputs captured before a project switch can unload
/// the session snapshot.
struct FocusedReviewTarget {
    folder: PathBuf,
    review_agent: ReviewAgent,
}

/// Action resumed after one full diff finishes loading.
enum SessionDiffPurpose {
    ApplyFocusedReview {
        auto_address: bool,
        cached_diff_hash: u64,
        suggestions: String,
    },
    Open {
        allow_empty: bool,
    },
    Review {
        cached_diff_hash: Option<u64>,
        is_manual: bool,
        target: FocusedReviewTarget,
    },
}

impl SessionDiffPurpose {
    /// Returns whether the request is validating a focused-review apply.
    fn is_apply_focused_review(&self) -> bool {
        matches!(self, Self::ApplyFocusedReview { .. })
    }

    /// Returns whether the request belongs to a focused-review continuation.
    fn is_review_action(&self) -> bool {
        matches!(self, Self::ApplyFocusedReview { .. } | Self::Review { .. })
    }
}

impl App {
    /// Starts loading a full diff and immediately switches to a cancelable
    /// loading page, preserving any composer or question restore state.
    pub(crate) fn start_diff_view_load(
        &mut self,
        session_id: &SessionId,
        restore: Option<DiffRestoreTarget>,
        sidebar_focus: DiffSidebarFocus,
        allow_empty: bool,
    ) -> bool {
        if matches!(&self.mode, AppMode::DiffLoading { session_id: loading_session_id, .. }
            if loading_session_id == session_id)
        {
            return true;
        }

        let fallback_view_scroll_offset = match &self.mode {
            AppMode::View {
                scroll_offset,
                session_id: viewed_session_id,
            } if viewed_session_id == session_id => *scroll_offset,
            _ => None,
        };
        let Some(request_id) =
            self.spawn_session_diff_request(session_id, SessionDiffPurpose::Open { allow_empty })
        else {
            if let Some(restore) = restore {
                self.mode = restore.into_mode();
            }

            return false;
        };

        self.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore: restore.map(Box::new),
            session_id: session_id.clone(),
            sidebar_focus,
        };

        true
    }

    /// Cancels a still-current interactive diff load and restores its source
    /// page. Dropping the request cancels its Git task and active subprocess.
    pub(crate) fn cancel_diff_view_load(&mut self) {
        let mode = std::mem::replace(&mut self.mode, AppMode::List);
        let AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore,
            session_id,
            ..
        } = mode
        else {
            self.mode = mode;

            return;
        };

        self.pending_session_diff_requests.remove(&request_id);
        self.mode = restore.map_or_else(
            || AppMode::View {
                scroll_offset: fallback_view_scroll_offset,
                session_id,
            },
            |restore| restore.into_mode(),
        );
    }

    /// Discards every diff continuation and deferred automatic-review trigger
    /// owned by a deleted session so detached task completions remain stale.
    pub(crate) fn discard_deleted_session_diff_state(&mut self, session_id: &SessionId) {
        self.auto_address_review_iterations.remove(session_id);
        self.diff_comment_progress.remove(session_id);
        self.pending_session_diff_requests
            .retain(|_, request| request.session_id != *session_id);
        self.deferred_auto_review_session_ids.remove(session_id);
    }

    /// Saves completed diff comments while their session's Diff mode is
    /// closed. Interaction-only selection state is reset before restoration.
    pub(crate) fn save_diff_comment_progress(
        &mut self,
        session_id: SessionId,
        mut line_comments: DiffLineComments,
    ) {
        line_comments.editing_index = None;
        line_comments.selection_anchor_index = None;
        line_comments.selected_comment_index = None;
        if line_comments.comments.is_empty() {
            self.diff_comment_progress.remove(&session_id);

            return;
        }

        self.diff_comment_progress.insert(session_id, line_comments);
    }

    /// Clears saved and currently displayed diff comments for one session
    /// after a new turn starts.
    pub(crate) fn clear_diff_comment_progress(&mut self, session_id: &str) {
        self.diff_comment_progress.remove(session_id);
        match &mut self.mode {
            AppMode::Diff {
                line_comments,
                scroll_cache,
                session_id: diff_session_id,
                ..
            } if diff_session_id == session_id => {
                *line_comments = DiffLineComments::default();
                *scroll_cache = None;
            }
            AppMode::Help {
                context:
                    crate::presentation::app_mode::HelpContext::Diff {
                        line_comments,
                        session_id: diff_session_id,
                        ..
                    },
                ..
            } if diff_session_id == session_id => {
                *line_comments = DiffLineComments::default();
            }
            _ => {}
        }
    }

    /// Persists and retains an automatic-review trigger for an eligible
    /// session that cannot start its review yet.
    pub(super) async fn defer_auto_review_session(&mut self, session_id: &SessionId) {
        self.persist_deferred_auto_review(DeferredAutoReviewPersistenceRetry::initial(
            session_id.clone(),
        ))
        .await;
    }

    /// Retries current automatic-review deferral writes after their bounded
    /// backoff delay.
    pub(super) async fn persist_deferred_auto_review_retries(
        &mut self,
        retries: Vec<DeferredAutoReviewPersistenceRetry>,
    ) {
        for retry in retries {
            if self
                .deferred_auto_review_session_ids
                .contains(&retry.session_id)
            {
                self.persist_deferred_auto_review(retry).await;
            }
        }
    }

    /// Applies one automatic-review deferral persistence attempt.
    async fn persist_deferred_auto_review(&mut self, retry: DeferredAutoReviewPersistenceRetry) {
        let result = self
            .services
            .db()
            .sessions()
            .defer_session_focused_review(retry.session_id.as_str())
            .await;
        Self::handle_deferred_auto_review_persistence_result(
            &mut self.deferred_auto_review_session_ids,
            self.services.event_sender(),
            retry,
            result,
        );
    }

    /// Retains failed triggers and schedules their next bounded persistence
    /// attempt through the foreground event reducer.
    fn handle_deferred_auto_review_persistence_result(
        deferred_session_ids: &mut HashSet<SessionId>,
        app_event_tx: tokio::sync::mpsc::UnboundedSender<crate::app::AppEvent>,
        retry: DeferredAutoReviewPersistenceRetry,
        result: Result<bool, DbError>,
    ) -> bool {
        let session_id = retry.session_id.clone();
        match result {
            Ok(true) => {
                deferred_session_ids.insert(session_id);

                false
            }
            Ok(false) => {
                deferred_session_ids.remove(&session_id);

                false
            }
            Err(error) => {
                deferred_session_ids.insert(session_id.clone());
                let Some(retry) = retry.next() else {
                    warn!(
                        session_id = %session_id,
                        error = %error,
                        "deferred automatic focused-review persistence retries exhausted; \
                         retaining the in-memory trigger"
                    );

                    return false;
                };
                warn!(
                    session_id = %session_id,
                    retry_attempt = retry.attempt,
                    %error,
                    "failed to persist deferred automatic focused review; scheduling retry"
                );
                TaskService::spawn_deferred_auto_review_persistence_retry(app_event_tx, retry);

                true
            }
        }
    }

    /// Starts one manual focused-review diff load unless that session already
    /// has a current request.
    pub(crate) fn start_manual_review_diff_load(&mut self, session_id: &SessionId) -> bool {
        self.start_review_diff_load(session_id, true)
    }

    /// Starts one focused-review freshness check without blocking prompt
    /// input or redraws on the full Git diff.
    pub(crate) fn start_apply_review_diff_load(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
    ) -> bool {
        self.start_apply_review_diff_load_with_mode(
            session_id,
            cached_diff_hash,
            suggestions,
            false,
        )
    }

    /// Starts one automatic focused-review freshness check.
    pub(crate) fn start_auto_apply_review_diff_load(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
    ) -> bool {
        self.start_apply_review_diff_load_with_mode(session_id, cached_diff_hash, suggestions, true)
    }

    fn start_apply_review_diff_load_with_mode(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
        auto_address: bool,
    ) -> bool {
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id && request.purpose.is_apply_focused_review()
        }) {
            return false;
        }

        self.spawn_session_diff_request(
            session_id,
            SessionDiffPurpose::ApplyFocusedReview {
                auto_address,
                cached_diff_hash,
                suggestions,
            },
        )
        .is_some()
    }

    /// Starts automatic review diff loads for eligible touched sessions.
    ///
    /// Requests are deduplicated per session, and existing generated output
    /// remains visible while its current diff hash is checked in the
    /// background.
    pub(super) fn start_auto_review_diff_loads(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            let Some(session) = self.sessions.session_for_id(session_id) else {
                continue;
            };
            let current_status = session.status;
            let session_role = session.role;

            if current_status == Status::InProgress {
                self.discard_pending_review_action_diff_loads(session_id);
                self.clear_review_output(session_id);

                continue;
            }
            if session_role == SessionRole::Orchestrator
                || !matches!(current_status, Status::Review | Status::AgentReview)
                || matches!(
                    self.review_cache.get(session_id),
                    Some(ReviewCacheEntry::Loading { .. } | ReviewCacheEntry::Suppressed)
                )
            {
                continue;
            }

            self.start_review_diff_load(session_id, false);
        }
    }

    /// Starts automatic review preparation for a review-ready session whose
    /// project is not currently loaded.
    pub(super) async fn start_inactive_auto_review_diff_load(
        &mut self,
        session_id: &SessionId,
    ) -> bool {
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id
                && matches!(&request.purpose, SessionDiffPurpose::Review { .. })
        }) || matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. } | ReviewCacheEntry::Suppressed)
        ) {
            return true;
        }

        let Ok(Some(row)) = self
            .services
            .db()
            .sessions()
            .load_session(session_id.as_str())
            .await
        else {
            return false;
        };
        let status = row.status.parse::<Status>().ok();
        let role = row
            .role
            .as_deref()
            .and_then(|value| value.parse::<SessionRole>().ok())
            .unwrap_or_default();
        if role == SessionRole::Orchestrator
            || !matches!(status, Some(Status::Review | Status::AgentReview))
        {
            return false;
        }
        let Some(project_id) = row.project_id else {
            return false;
        };
        let review_agent =
            crate::app::setting::load_default_review_agent_setting(&self.services, project_id)
                .await;
        let folder = session::session_folder(self.services.base_path(), session_id.as_str());
        let source = SessionDiffTaskSource::Worktree {
            archived_fallback: None,
            base_branch: row.base_branch,
            git_client: self.services.git_client(),
        };

        self.defer_auto_review_session(session_id).await;

        self.start_review_diff_load_for_target(
            session_id,
            false,
            FocusedReviewTarget {
                folder: folder.clone(),
                review_agent,
            },
            folder,
            source,
        )
    }

    /// Invalidates review and apply continuations captured before newly
    /// completed turns, then clears their stale focused-review generations.
    pub(super) fn supersede_review_diff_loads(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            self.discard_pending_review_action_diff_loads(session_id);
            self.clear_review_output(session_id);
            review::restore_session_review_status(self.sessions.state_mut(), session_id);
        }
    }

    /// Invalidates `/apply` diff continuations captured from review output
    /// that is being cleared or replaced.
    pub(super) fn discard_pending_apply_review_diff_loads(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id || !request.purpose.is_apply_focused_review()
        });
    }

    /// Applies one completed diff only when its request generation and
    /// session still match the foreground continuation.
    pub(crate) async fn apply_session_diff_update(&mut self, update: SessionDiffUpdate) {
        let request = match self.pending_session_diff_requests.entry(update.request_id) {
            Entry::Occupied(entry) if entry.get().session_id == update.session_id => entry.remove(),
            Entry::Occupied(_) | Entry::Vacant(_) => return,
        };

        match request.purpose {
            SessionDiffPurpose::ApplyFocusedReview {
                auto_address,
                cached_diff_hash,
                suggestions,
            } => {
                self.apply_focused_review_diff_update(
                    &update.session_id,
                    auto_address,
                    cached_diff_hash,
                    &suggestions,
                    update.result,
                )
                .await;
            }
            SessionDiffPurpose::Open { allow_empty } => {
                self.apply_open_diff_update(update, allow_empty);
            }
            SessionDiffPurpose::Review {
                cached_diff_hash,
                is_manual,
                target,
            } => {
                self.apply_review_diff_update(update, cached_diff_hash, is_manual, target)
                    .await;
            }
        }
    }

    /// Completes `/apply` only when the background diff still matches the
    /// focused-review generation selected by the user.
    async fn apply_focused_review_diff_update(
        &mut self,
        session_id: &SessionId,
        auto_address: bool,
        cached_diff_hash: u64,
        suggestions: &str,
        result: Result<String, String>,
    ) {
        let Some(current_review_text) =
            self.review_cache
                .get(session_id)
                .and_then(|entry| match entry {
                    ReviewCacheEntry::Ready { diff_hash, text }
                        if *diff_hash == cached_diff_hash =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
        else {
            return;
        };
        let review_generation_is_current = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.status == Status::Review);
        if !review_generation_is_current {
            return;
        }

        let current_diff = match result {
            Ok(diff) => diff,
            Err(error) => {
                self.append_prompt_status_line(
                    session_id,
                    crate::domain::transcript_notice::TranscriptNotice::Apply,
                    &format!(
                        "Failed to read worktree diff: {error}. Review cache preserved; try \
                         /apply again."
                    ),
                )
                .await;

                return;
            }
        };
        if review::diff_content_hash(&current_diff) != cached_diff_hash {
            self.clear_review_output(session_id);
            self.append_prompt_status_line(
                session_id,
                crate::domain::transcript_notice::TranscriptNotice::Apply,
                "Review is stale; the worktree changed since it was generated. Run focused review \
                 again (f key).",
            )
            .await;

            return;
        }

        let completed_auto_address_iterations = if auto_address {
            let auto_address_mode_is_current = self
                .sessions
                .session_for_id(session_id)
                .is_some_and(|session| {
                    session.permission_mode
                        == crate::domain::permission::PermissionMode::AutoEditAddressComments
                });
            let completed_iterations = self
                .auto_address_review_iterations
                .get(session_id)
                .copied()
                .unwrap_or(0);
            if !auto_address_mode_is_current
                || completed_iterations
                    >= crate::app::prompt_intent::MAX_AUTO_ADDRESS_REVIEW_ITERATIONS
            {
                return;
            }

            Some(completed_iterations)
        } else {
            None
        };

        let prompt = crate::app::prompt_intent::build_apply_review_prompt(suggestions);
        if let Some(completed_iterations) = completed_auto_address_iterations {
            if !self.reply(session_id, prompt).await {
                self.set_review_ready_output(
                    session_id,
                    cached_diff_hash,
                    current_review_text.clone(),
                );
                self.persist_focused_review_updates(vec![FocusedReviewPersistence {
                    diff_hash: Some(cached_diff_hash),
                    session_id: session_id.clone(),
                    status: FocusedReviewStatus::Ready,
                    text: Some(current_review_text),
                }])
                .await;

                return;
            }

            self.auto_address_review_iterations
                .insert(session_id.clone(), completed_iterations.saturating_add(1));

            return;
        }

        self.reply(session_id, prompt).await;
    }

    /// Discards detached diff tasks whose continuations belong to an obsolete
    /// focused-review turn. Their eventual events are ignored as stale.
    fn discard_pending_review_action_diff_loads(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id || !request.purpose.is_review_action()
        });
    }

    /// Spawns the appropriate archive or worktree diff source and registers
    /// the continuation before the foreground task yields again.
    fn spawn_session_diff_request(
        &mut self,
        session_id: &SessionId,
        purpose: SessionDiffPurpose,
    ) -> Option<u64> {
        let session = self.sessions.session_for_id(session_id)?;
        let (folder, source) = self.session_diff_task_target(session);

        Some(self.spawn_session_diff_request_for_target(session_id, purpose, folder, source))
    }

    /// Captures one loaded session's folder and archive-or-worktree diff
    /// source.
    fn session_diff_task_target(&self, session: &Session) -> (PathBuf, SessionDiffTaskSource) {
        let source = if session.is_managed()
            && (session.status == Status::Done
                || (session.role == SessionRole::OrchestrationResearcher
                    && session.status == Status::Canceled))
        {
            SessionDiffTaskSource::Archived {
                repositories: self.services.db().clone(),
            }
        } else {
            SessionDiffTaskSource::Worktree {
                archived_fallback: (session.is_managed() && session.status == Status::Merging)
                    .then(|| self.services.db().clone()),
                base_branch: session.base_branch.clone(),
                git_client: self.services.git_client(),
            }
        };

        (session.folder.clone(), source)
    }

    /// Spawns one diff task from captured session metadata and registers its
    /// foreground continuation.
    fn spawn_session_diff_request_for_target(
        &mut self,
        session_id: &SessionId,
        purpose: SessionDiffPurpose,
        folder: PathBuf,
        source: SessionDiffTaskSource,
    ) -> u64 {
        let cancellation = CancellationToken::new();
        let input = SessionDiffTaskInput {
            cancellation: cancellation.clone(),
            app_event_tx: self.services.event_sender(),
            folder,
            session_id: session_id.clone(),
            source,
        };
        let request_id = TaskService::spawn_session_diff_task(input);
        self.pending_session_diff_requests.insert(
            request_id,
            PendingSessionDiffRequest {
                _cancellation: cancellation.drop_guard(),
                purpose,
                session_id: session_id.clone(),
            },
        );

        request_id
    }

    /// Starts one deduplicated review-preparation request and shows loading
    /// state only when there is no prior review output to retain.
    fn start_review_diff_load(&mut self, session_id: &SessionId, is_manual: bool) -> bool {
        let review_agent = review::normalize_review_agent(self.review_agent());
        let Some(session) = self.sessions.session_for_id(session_id) else {
            return false;
        };
        let (folder, source) = self.session_diff_task_target(session);

        self.start_review_diff_load_for_target(
            session_id,
            is_manual,
            FocusedReviewTarget {
                folder: folder.clone(),
                review_agent,
            },
            folder,
            source,
        )
    }

    /// Starts one review-preparation request from stable target metadata.
    fn start_review_diff_load_for_target(
        &mut self,
        session_id: &SessionId,
        is_manual: bool,
        target: FocusedReviewTarget,
        folder: PathBuf,
        source: SessionDiffTaskSource,
    ) -> bool {
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id
                && matches!(&request.purpose, SessionDiffPurpose::Review { .. })
        }) {
            return false;
        }

        let cached_diff_hash = self
            .review_cache
            .get(session_id)
            .and_then(ReviewCacheEntry::diff_hash);
        let review_agent = target.review_agent;
        self.spawn_session_diff_request_for_target(
            session_id,
            SessionDiffPurpose::Review {
                cached_diff_hash,
                is_manual,
                target,
            },
            folder,
            source,
        );

        if is_manual && cached_diff_hash.is_none() {
            self.review_cache.insert(
                session_id.clone(),
                ReviewCacheEntry::Loading {
                    diff_hash: review::diff_content_hash(""),
                    review_agent,
                },
            );
            review::mark_session_agent_review(self.sessions.state_mut(), session_id);
            if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
                session.transient_messages.upsert(TransientMessage {
                    anchor: TransientMessageAnchor::Tail,
                    body: TransientMessageBody::Loading(review::review_loading_message(
                        review_agent,
                    )),
                    lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                    slot: TransientMessageSlot::Review,
                    turn_position: session.latest_user_prompt_position(),
                });
            }
        }

        true
    }

    /// Replaces a matching diff-loading page with the loaded workspace or its
    /// preserved source page when unchanged sessions do not expose `d`.
    fn apply_open_diff_update(&mut self, update: SessionDiffUpdate, allow_empty: bool) {
        let mode = std::mem::replace(&mut self.mode, AppMode::List);
        let AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore,
            session_id,
            sidebar_focus,
        } = mode
        else {
            self.mode = mode;

            return;
        };
        if request_id != update.request_id || session_id != update.session_id {
            self.mode = AppMode::DiffLoading {
                fallback_view_scroll_offset,
                request_id,
                restore,
                session_id,
                sidebar_focus,
            };

            return;
        }

        let diff = match update.result {
            Ok(diff) => diff,
            Err(error) => {
                self.mode = restore.map_or_else(
                    || AppMode::View {
                        scroll_offset: fallback_view_scroll_offset,
                        session_id: session_id.clone(),
                    },
                    |restore| restore.into_mode(),
                );
                self.sessions.append_workflow_notice(
                    &session_id,
                    crate::domain::transcript_notice::TranscriptNotice::Error
                        .format_line(format!("Unable to load diff: {error}")),
                );

                return;
            }
        };
        if diff.trim().is_empty() && !allow_empty {
            self.mode = restore.map_or_else(
                || AppMode::View {
                    scroll_offset: fallback_view_scroll_offset,
                    session_id,
                },
                |restore| restore.into_mode(),
            );

            return;
        }

        let mut review_comments = self.start_session_review_comment_load(&session_id);
        if let Some(review_comments) = &mut review_comments {
            review_comments.sidebar_focus = sidebar_focus;
        }
        self.mode = AppMode::Diff {
            diff,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: self
                .diff_comment_progress
                .remove(&session_id)
                .unwrap_or_default(),
            preview: DiffPreview::default(),
            review_comments,
            restore,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id,
        };
    }

    /// Validates and starts focused review generation from one background
    /// diff result, preserving cache generations when the result is stale.
    async fn apply_review_diff_update(
        &mut self,
        update: SessionDiffUpdate,
        cached_diff_hash: Option<u64>,
        is_manual: bool,
        target: FocusedReviewTarget,
    ) {
        let session_id = update.session_id;
        let status = if let Some(session) = self.sessions.session_for_id(&session_id) {
            Some(session.status)
        } else {
            self.services
                .db()
                .sessions()
                .load_session(session_id.as_str())
                .await
                .ok()
                .flatten()
                .and_then(|row| row.status.parse::<Status>().ok())
        };
        if !matches!(status, Some(Status::Review | Status::AgentReview)) {
            if cached_diff_hash.is_none() {
                self.clear_review_output(&session_id);
            }

            return;
        }
        self.deferred_auto_review_session_ids.remove(&session_id);
        let diff = match update.result {
            Ok(diff) if !diff.starts_with("Failed to run git diff:") => diff,
            Ok(error) | Err(error) => {
                let persistence = review::fail_review_preparation(
                    &mut self.review_cache,
                    self.sessions.state_mut(),
                    &session_id,
                    error,
                );
                self.persist_focused_review_updates(vec![persistence]).await;
                review::restore_session_review_status(self.sessions.state_mut(), &session_id);

                return;
            }
        };
        let diff_hash = review::diff_content_hash(&diff);
        if diff.trim().is_empty() {
            if is_manual {
                let _ = self
                    .services
                    .db()
                    .sessions()
                    .update_session_focused_review(&session_id, None, None, None)
                    .await;
                self.set_review_ready_output(
                    &session_id,
                    diff_hash,
                    review::REVIEW_NO_DIFF_MESSAGE.to_string(),
                );
            } else if cached_diff_hash.is_none() {
                let _ = self
                    .services
                    .db()
                    .sessions()
                    .update_session_focused_review(&session_id, None, None, None)
                    .await;
                self.clear_review_output(&session_id);
            }
            review::restore_session_review_status(self.sessions.state_mut(), &session_id);

            return;
        }
        if cached_diff_hash == Some(diff_hash) {
            review::restore_session_review_status(self.sessions.state_mut(), &session_id);

            return;
        }

        self.start_review_assist(
            &session_id,
            &target.folder,
            diff_hash,
            &diff,
            target.review_agent,
        )
        .await;
        self.persist_focused_review_updates(vec![FocusedReviewPersistence {
            diff_hash: Some(diff_hash),
            session_id,
            status: FocusedReviewStatus::Pending,
            text: None,
        }])
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::input::InputState;
    use crate::domain::session::{
        ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    };
    use crate::presentation::app_mode::PromptModeSnapshot;
    use crate::presentation::prompt::{
        PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };

    /// Builds one Git-backed review session for diff-request state tests.
    async fn review_app() -> (App, tempfile::TempDir, SessionId) {
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from(
            app.create_session()
                .await
                .expect("session should be created"),
        );
        app.sessions.sessions_mut()[0].status = Status::Review;

        (app, base_dir, session_id)
    }

    /// Returns the active loading request generation.
    fn loading_request_id(app: &App) -> Option<u64> {
        match app.mode {
            AppMode::DiffLoading { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    /// Attaches one open review request to the created session.
    fn attach_review_request(app: &mut App) {
        app.sessions.sessions_mut()[0].review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-diff".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Session diff".to_string(),
                web_url: "https://example.test/pull/42".to_string(),
            },
        });
    }

    /// Builds stable review inputs for direct diff-completion tests.
    fn test_review_target(app: &App, session_id: &SessionId) -> FocusedReviewTarget {
        let folder = app.sessions.session_for_id(session_id).map_or_else(
            || session::session_folder(app.services.base_path(), session_id.as_str()),
            |session| session.folder.clone(),
        );

        FocusedReviewTarget {
            folder,
            review_agent: app.review_agent(),
        }
    }

    #[tokio::test]
    async fn cancel_diff_view_load_restores_view_and_discards_completion() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.mode = AppMode::View {
            scroll_offset: Some(7),
            session_id: session_id.clone(),
        };
        assert_eq!(loading_request_id(&app), None);
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false));
        assert_eq!(loading_request_id(&app), Some(request_id));
        assert_eq!(app.pending_session_diff_requests.len(), 1);

        // Act
        app.cancel_diff_view_load();
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;
        app.cancel_diff_view_load();

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref viewed_session_id,
                scroll_offset: Some(7),
            } if viewed_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn stale_open_diff_completion_keeps_newer_loading_generation() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
        let newer_request_id = request_id.saturating_add(1);
        app.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: newer_request_id,
            restore: None,
            session_id: session_id.clone(),
            sidebar_focus: DiffSidebarFocus::Comments,
        };

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::DiffLoading {
                request_id,
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            } if request_id == newer_request_id
        ));
    }

    #[tokio::test]
    async fn open_diff_completion_after_mode_change_is_discarded() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
        app.mode = AppMode::List;

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.pending_session_diff_requests.is_empty());
    }

    #[tokio::test]
    async fn open_diff_completion_preserves_comment_focus() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        attach_review_request(&mut app);
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Comments, true,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff --git a/file b/file\n+change".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(crate::presentation::app_mode::DiffReviewComments {
                    sidebar_focus: DiffSidebarFocus::Comments,
                    ..
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn open_diff_failure_restores_prompt_snapshot() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        let restore = DiffRestoreTarget::Prompt(PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::default(),
            input: InputState::with_text("preserved draft".to_string()),
            scroll_offset: Some(5),
            session_id: session_id.clone(),
            slash_state: PromptSlashState::default(),
        });
        assert!(app.start_diff_view_load(
            &session_id,
            Some(restore),
            DiffSidebarFocus::Files,
            false,
        ));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Err("worktree unavailable".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                input,
                scroll_offset: Some(5),
                session_id: restored_session_id,
                ..
            } if input.text() == "preserved draft" && restored_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn mismatched_session_diff_completion_is_ignored() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("wrong session".to_string()),
            session_id: "other-session".into(),
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::DiffLoading { .. }));
        assert!(app.pending_session_diff_requests.contains_key(&request_id));
    }

    #[tokio::test]
    async fn missing_session_cannot_start_diff_requests() {
        // Arrange
        let (mut app, _base_dir, _session_id) = review_app().await;
        let missing_session_id = SessionId::from("missing-session");

        // Act
        let diff_started =
            app.start_diff_view_load(&missing_session_id, None, DiffSidebarFocus::Files, false);
        let apply_started =
            app.start_apply_review_diff_load(&missing_session_id, 1, "suggestion".to_string());
        let review_started = app.start_manual_review_diff_load(&missing_session_id);

        // Assert
        assert!(!diff_started);
        assert!(!apply_started);
        assert!(!review_started);
    }

    #[tokio::test]
    async fn apply_review_diff_request_is_deduplicated_per_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;

        // Act
        let first_started =
            app.start_apply_review_diff_load(&session_id, 1, "first suggestion".to_string());
        let second_started =
            app.start_apply_review_diff_load(&session_id, 1, "duplicate suggestion".to_string());

        // Assert
        assert!(first_started);
        assert!(!second_started);
        assert_eq!(app.pending_session_diff_requests.len(), 1);
        assert!(app.pending_session_diff_requests.values().any(|request| {
            request.session_id == session_id
                && matches!(
                    &request.purpose,
                    SessionDiffPurpose::ApplyFocusedReview { .. }
                )
        }));
    }

    #[tokio::test]
    async fn clearing_review_output_discards_pending_apply_request() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash: 1,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(&session_id, 1, "- Fix the issue.".to_string(),));

        // Act
        app.clear_review_output(&session_id);

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn automatic_review_diff_load_ignores_missing_loaded_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let session_ids = HashSet::from([SessionId::from("missing-session")]);

        // Act
        app.start_auto_review_diff_loads(&session_ids);

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(app.review_cache.is_empty());
    }

    #[tokio::test]
    async fn inactive_auto_review_diff_load_accepts_existing_request() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let session_id = SessionId::from("inactive-review-pending");
        let target = test_review_target(&app, &session_id);
        app.pending_session_diff_requests.insert(
            42,
            PendingSessionDiffRequest {
                _cancellation: CancellationToken::new().drop_guard(),
                purpose: SessionDiffPurpose::Review {
                    cached_diff_hash: None,
                    is_manual: false,
                    target,
                },
                session_id: session_id.clone(),
            },
        );

        // Act
        let accepted = app.start_inactive_auto_review_diff_load(&session_id).await;

        // Assert
        assert!(accepted);
        assert_eq!(app.pending_session_diff_requests.len(), 1);
    }

    #[tokio::test]
    async fn inactive_auto_review_diff_load_rejects_invalid_persisted_metadata() {
        // Arrange
        let (mut app, _base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let project_id = app.projects.active_project_id();
        let invalid_status_id = SessionId::from("inactive-invalid-status");
        let inactive_status_id = SessionId::from("inactive-done-status");
        let missing_project_id = SessionId::from("inactive-missing-project");
        for (session_id, status) in [
            (&invalid_status_id, "Review"),
            (&inactive_status_id, "Done"),
            (&missing_project_id, "Review"),
        ] {
            app.services
                .db()
                .sessions()
                .insert_session(
                    session_id.as_str(),
                    "gpt-5.6-sol",
                    "main",
                    status,
                    project_id,
                )
                .await
                .expect("failed to insert inactive session fixture");
        }
        sqlx::query("UPDATE session SET status = 'Unknown' WHERE id = ?")
            .bind(invalid_status_id.as_str())
            .execute(&pool)
            .await
            .expect("failed to invalidate inactive session status");
        sqlx::query("UPDATE session SET project_id = NULL WHERE id = ?")
            .bind(missing_project_id.as_str())
            .execute(&pool)
            .await
            .expect("failed to clear inactive session project");

        // Act
        let invalid_status_started = app
            .start_inactive_auto_review_diff_load(&invalid_status_id)
            .await;
        let inactive_status_started = app
            .start_inactive_auto_review_diff_load(&inactive_status_id)
            .await;
        let missing_project_started = app
            .start_inactive_auto_review_diff_load(&missing_project_id)
            .await;

        // Assert
        assert!(!invalid_status_started);
        assert!(!inactive_status_started);
        assert!(!missing_project_started);
        assert!(app.pending_session_diff_requests.is_empty());
    }

    #[tokio::test]
    async fn automatic_review_diff_completion_continues_for_inactive_project() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let repositories = app.services.db().clone();
        let session_id = SessionId::from("inactive-review-diff");
        let inactive_project_path = base_dir.path().join("inactive-project");
        let inactive_project_id = repositories
            .projects()
            .upsert_project(&inactive_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert inactive project");
        repositories
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive review session");
        let update = SessionDiffUpdate {
            request_id: 1,
            result: Ok("diff --git a/file b/file".to_string()),
            session_id: session_id.clone(),
        };

        // Act
        let target = test_review_target(&app, &session_id);
        app.apply_review_diff_update(update, None, false, target)
            .await;
        let review_is_loading = matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Loading { .. })
        );
        drop(app);
        let recoverable_session_ids = repositories
            .sessions()
            .load_pending_focused_review_session_ids(inactive_project_id)
            .await
            .expect("failed to recover deferred review after restart");

        // Assert
        assert!(review_is_loading);
        assert_eq!(recoverable_session_ids, [session_id.as_str()]);
    }

    #[tokio::test]
    async fn transient_deferred_auto_review_persistence_failure_retains_and_retries_trigger() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let session_id = SessionId::from("retry-deferred-review");
        let inactive_project_path = base_dir.path().join("inactive-project");
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project(&inactive_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert inactive project");
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive review session");
        let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
        let retry_scheduled = App::handle_deferred_auto_review_persistence_result(
            &mut app.deferred_auto_review_session_ids,
            retry_tx,
            DeferredAutoReviewPersistenceRetry::initial(session_id.clone()),
            Err(DbError::Query(sqlx::Error::PoolClosed)),
        );

        // Act
        let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
            .await
            .expect("timed out waiting for deferred review persistence retry")
            .expect("deferred review persistence failure should requeue an event");
        app.apply_app_events(retry_event).await;
        let recoverable_session_ids = app
            .services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(inactive_project_id)
            .await
            .expect("failed to load retried deferred review");
        app.services
            .db()
            .sessions()
            .update_session_focused_review(session_id.as_str(), None, None, None)
            .await
            .expect("failed to clear retried deferred review");
        app.deferred_auto_review_session_ids.remove(&session_id);
        app.apply_app_events(crate::app::AppEvent::DeferredAutoReviewPersistenceRetry {
            retry: DeferredAutoReviewPersistenceRetry {
                attempt: 2,
                session_id: session_id.clone(),
            },
        })
        .await;
        let stale_retry_session_ids = app
            .services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(inactive_project_id)
            .await
            .expect("failed to check stale deferred review retry");
        let (exhausted_tx, mut exhausted_rx) = tokio::sync::mpsc::unbounded_channel();
        let exhausted_retry_scheduled = App::handle_deferred_auto_review_persistence_result(
            &mut app.deferred_auto_review_session_ids,
            exhausted_tx,
            DeferredAutoReviewPersistenceRetry {
                attempt: MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES,
                session_id: session_id.clone(),
            },
            Err(DbError::Query(sqlx::Error::PoolClosed)),
        );
        let exhausted_event = exhausted_rx.recv().await;

        // Assert
        assert!(retry_scheduled);
        assert!(!exhausted_retry_scheduled);
        assert_eq!(exhausted_event, None);
        assert!(app.deferred_auto_review_session_ids.contains(&session_id));
        assert_eq!(recoverable_session_ids, [session_id.as_str()]);
        assert_eq!(stale_retry_session_ids, [] as [String; 0]);
    }

    #[tokio::test]
    async fn automatic_empty_review_diff_clears_durable_trigger() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at(session_id.as_str(), "Review", 1)
            .await
            .expect("failed to persist review status");
        assert!(
            app.services
                .db()
                .sessions()
                .defer_session_focused_review(session_id.as_str())
                .await
                .expect("failed to persist deferred review")
        );
        let project_id = app.projects.active_project_id();
        let update = SessionDiffUpdate {
            request_id: 1,
            result: Ok(String::new()),
            session_id: session_id.clone(),
        };

        // Act
        let target = test_review_target(&app, &session_id);
        app.apply_review_diff_update(update, None, false, target)
            .await;

        // Assert
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_pending_focused_review_session_ids(project_id)
                .await
                .expect("failed to load pending reviews"),
            [] as [String; 0]
        );
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn automatic_empty_review_diff_preserves_cached_output() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        let cached_diff_hash = 42;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash: cached_diff_hash,
                text: "## Review\nExisting finding.".to_string(),
            },
        );
        let update = SessionDiffUpdate {
            request_id: 1,
            result: Ok(String::new()),
            session_id: session_id.clone(),
        };
        let target = test_review_target(&app, &session_id);

        // Act
        app.apply_review_diff_update(update, Some(cached_diff_hash), false, target)
            .await;

        // Assert
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Ready { diff_hash, text })
                if *diff_hash == cached_diff_hash && text.contains("Existing finding")
        ));
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    }

    #[tokio::test]
    async fn deleting_session_discards_pending_review_diff_and_late_completion() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        let request_id = 42;
        app.pending_session_diff_requests.insert(
            request_id,
            PendingSessionDiffRequest {
                _cancellation: CancellationToken::new().drop_guard(),
                purpose: SessionDiffPurpose::Review {
                    cached_diff_hash: None,
                    is_manual: false,
                    target: test_review_target(&app, &session_id),
                },
                session_id: session_id.clone(),
            },
        );
        app.deferred_auto_review_session_ids
            .insert(session_id.clone());

        // Act
        app.delete_selected_session().await;
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("late diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.deferred_auto_review_session_ids.contains(&session_id));
    }

    #[tokio::test]
    async fn deferred_cleanup_deletion_discards_pending_review_diff_state() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.pending_session_diff_requests.insert(
            42,
            PendingSessionDiffRequest {
                _cancellation: CancellationToken::new().drop_guard(),
                purpose: SessionDiffPurpose::Review {
                    cached_diff_hash: None,
                    is_manual: false,
                    target: test_review_target(&app, &session_id),
                },
                session_id: session_id.clone(),
            },
        );
        app.deferred_auto_review_session_ids
            .insert(session_id.clone());

        // Act
        app.delete_selected_session_deferred_cleanup().await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.deferred_auto_review_session_ids.contains(&session_id));
    }

    #[tokio::test]
    async fn apply_completion_ignores_replaced_review_generation() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("apply diff request should be pending");
        let review_agent = app.review_agent();
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading {
                diff_hash,
                review_agent,
            },
        );

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Loading {
                diff_hash: cached_hash,
                ..
            })
                if *cached_hash == diff_hash
        ));
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    }

    #[tokio::test]
    async fn apply_completion_ignores_session_that_left_review() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("apply diff request should be pending");
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Ready {
                diff_hash: cached_hash,
                ..
            }) if *cached_hash == diff_hash
        ));
        assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    }

    #[tokio::test]
    async fn manual_apply_completion_enqueues_remediation_turn() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("manual apply diff request should be pending");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(!app.auto_address_review_iterations.contains_key(&session_id));
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn automatic_apply_completion_counts_enqueued_remediation_turn() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.sessions.sessions_mut()[0].permission_mode =
            crate::domain::permission::PermissionMode::AutoEditAddressComments;
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_auto_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("automatic apply diff request should be pending");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert_eq!(
            app.auto_address_review_iterations.get(&session_id),
            Some(&1)
        );
        assert!(app.pending_session_diff_requests.is_empty());
    }

    #[tokio::test]
    async fn automatic_apply_completion_does_not_count_failed_remediation_enqueue() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.sessions.sessions_mut()[0].permission_mode =
            crate::domain::permission::PermissionMode::AutoEditAddressComments;
        app.auto_address_review_iterations
            .insert(session_id.clone(), 2);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_auto_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("automatic apply diff request should be pending");
        app.sessions.session_handles_mut().remove(&session_id);

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert_eq!(
            app.auto_address_review_iterations.get(&session_id),
            Some(&2)
        );
        assert!(app.pending_session_diff_requests.is_empty());
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Ready { diff_hash: cached_diff_hash, text })
                if *cached_diff_hash == diff_hash && text.contains("Fix the issue")
        ));
        let visible_review_text = app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::Review)
            .map(|message| message.body.text());
        assert!(visible_review_text.is_some_and(|text| text.contains("Fix the issue")));
        let persisted_reviews = app
            .services
            .db()
            .sessions()
            .load_session_focused_reviews_for_project(app.active_project_id())
            .await
            .expect("failed to load restored focused review");
        assert!(persisted_reviews.iter().any(|review| {
            review.session_id == session_id.as_str()
                && review.diff_hash == diff_hash.to_string()
                && review.text.contains("Fix the issue")
        }));
    }

    #[tokio::test]
    async fn automatic_apply_completion_revalidates_mode_and_iteration_limit() {
        // Arrange, Act, Assert
        for (permission_mode, completed_iterations) in [
            (crate::domain::permission::PermissionMode::AutoEdit, 0),
            (
                crate::domain::permission::PermissionMode::AutoEditAddressComments,
                crate::app::prompt_intent::MAX_AUTO_ADDRESS_REVIEW_ITERATIONS,
            ),
        ] {
            let (mut app, _base_dir, session_id) = review_app().await;
            crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
            app.sessions.sessions_mut()[0].permission_mode = permission_mode;
            app.auto_address_review_iterations
                .insert(session_id.clone(), completed_iterations);
            let current_diff = String::new();
            let diff_hash = review::diff_content_hash(&current_diff);
            app.review_cache.insert(
                session_id.clone(),
                ReviewCacheEntry::Ready {
                    diff_hash,
                    text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
                },
            );
            assert!(app.start_auto_apply_review_diff_load(
                &session_id,
                diff_hash,
                "- Fix the issue.".to_string(),
            ));
            let request_id = *app
                .pending_session_diff_requests
                .keys()
                .next()
                .expect("automatic apply diff request should be pending");

            app.apply_session_diff_update(SessionDiffUpdate {
                request_id,
                result: Ok(current_diff),
                session_id: session_id.clone(),
            })
            .await;

            assert_eq!(
                app.auto_address_review_iterations.get(&session_id),
                Some(&completed_iterations)
            );
            assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        }
    }

    #[tokio::test]
    async fn review_diff_request_is_deduplicated_and_cleared_after_status_change() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;

        // Act
        let first_started = app.start_manual_review_diff_load(&session_id);
        let second_started = app.start_manual_review_diff_load(&session_id);
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(first_started);
        assert!(!second_started);
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn superseding_review_turn_discards_review_action_requests() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        assert!(app.start_apply_review_diff_load(&session_id, 1, "suggestion".to_string()));
        let completed_sessions = HashSet::from([session_id.clone()]);

        // Act
        app.supersede_review_diff_loads(&completed_sessions);

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn review_completion_for_removed_session_clears_loading_cache() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");
        app.sessions.remove_session_at(0);

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn review_diff_failure_restores_review_status() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Err("Failed to run git diff: unavailable".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Failed { error, .. }) if error.contains("unavailable")
        ));
    }
}
