//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use time::{OffsetDateTime, UtcOffset};

use super::session_folder;
use crate::app::{AppServices, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::session::{
    DailyActivity, Session, SessionHandles, SessionSize, SessionStats, Status,
};
use crate::infra::db::Database;
use crate::infra::git::GitClient;

const SECONDS_PER_DAY: i64 = 86_400;

impl SessionManager {
    /// Loads session models from the database and reuses live handles when
    /// possible.
    ///
    /// Existing handles are reused in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    ///
    /// When a handle already exists, live handle output is treated as
    /// authoritative for the returned in-memory snapshot to avoid clobbering
    /// fresh runtime output with stale persisted rows. Active statuses are also
    /// preserved from live handles, while terminal persisted statuses (`Done`,
    /// `Canceled`) override stale in-memory status.
    ///
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Returns both loaded sessions and local-day activity counts aggregated
    /// from persisted session-creation activity history.
    pub(crate) async fn load_sessions(
        base: &Path,
        db: &Database,
        active_project_id: i64,
        working_dir: &Path,
        handles: &mut HashMap<String, SessionHandles>,
    ) -> (Vec<Session>, Vec<DailyActivity>) {
        let project_name = working_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let db_rows = db
            .load_sessions_for_project(active_project_id)
            .await
            .unwrap_or_default();
        let activity_timestamps = db
            .load_session_activity_timestamps()
            .await
            .unwrap_or_default();
        let mut sessions: Vec<Session> = Vec::new();

        for row in db_rows {
            let folder = session_folder(base, &row.id);
            let persisted_status = row.status.parse::<Status>().unwrap_or(Status::Done);
            let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
            let persisted_status_is_terminal =
                matches!(persisted_status, Status::Done | Status::Canceled);
            if !folder.is_dir() && !persisted_status_is_terminal {
                continue;
            }
            let session_model = row
                .model
                .parse::<AgentModel>()
                .unwrap_or_else(|_| AgentKind::Gemini.default_model());

            let mut status = persisted_status;
            let mut output = row.output.clone();
            if let Some(existing) = handles.get(&row.id) {
                if let Ok(output_buffer) = existing.output.lock() {
                    output.clone_from(&output_buffer);
                }

                if let Ok(mut live_status) = existing.status.lock() {
                    if persisted_status_is_terminal {
                        *live_status = persisted_status;
                    } else {
                        status = *live_status;
                    }
                }
            } else {
                handles.insert(
                    row.id.clone(),
                    SessionHandles::new(row.output.clone(), persisted_status),
                );
            }

            sessions.push(Session {
                base_branch: row.base_branch,
                created_at: row.created_at,
                folder,
                id: row.id,
                model: session_model,
                output,
                project_name: project_name.clone(),
                prompt: row.prompt,
                size: persisted_size,
                stats: SessionStats {
                    input_tokens: row.input_tokens.cast_unsigned(),
                    output_tokens: row.output_tokens.cast_unsigned(),
                },
                status,
                summary: row.summary,
                title: row.title,
                updated_at: row.updated_at,
            });
        }

        let stats_activity = aggregate_local_daily_activity(&activity_timestamps);

        (sessions, stats_activity)
    }

    /// Recomputes git-diff size for one session and persists it.
    ///
    /// This is invoked explicitly by session-open and turn-complete flows,
    /// keeping list refreshes free of per-session git diff work.
    pub(crate) async fn refresh_session_size(&mut self, services: &AppServices, session_id: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let (base_branch, folder) = {
            let session = &self.sessions[session_index];
            (session.base_branch.clone(), session.folder.clone())
        };
        let computed_size =
            Self::session_size_for_folder(services.git_client().as_ref(), &folder, &base_branch)
                .await;
        let _ = services
            .db()
            .update_session_size(session_id, &computed_size.to_string())
            .await;

        if let Some(session) = self.sessions.get_mut(session_index) {
            session.size = computed_size;
        }
    }

    /// Computes session-size bucket from one worktree folder's diff.
    pub(crate) async fn session_size_for_folder(
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
    ) -> SessionSize {
        if !folder.is_dir() {
            return SessionSize::Xs;
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let diff = git_client
            .diff(folder, base_branch)
            .await
            .ok()
            .unwrap_or_default();

        SessionSize::from_diff(&diff)
    }
}

/// Aggregates persisted session-creation timestamps into local-day totals.
fn aggregate_local_daily_activity(activity_timestamps: &[i64]) -> Vec<DailyActivity> {
    let mut activity_by_day: BTreeMap<i64, u32> = BTreeMap::new();

    for timestamp_seconds in activity_timestamps {
        let day_key = activity_day_key_local(*timestamp_seconds);
        let day_count = activity_by_day.entry(day_key).or_insert(0);
        *day_count = day_count.saturating_add(1);
    }

    activity_by_day
        .into_iter()
        .map(|(day_key, session_count)| DailyActivity {
            day_key,
            session_count,
        })
        .collect()
}

/// Converts a Unix timestamp into a local day key.
///
/// The offset is resolved at the event timestamp so DST transitions are
/// reflected in the day-key projection.
fn activity_day_key_local(timestamp_seconds: i64) -> i64 {
    let utc_offset_seconds = local_utc_offset_seconds(timestamp_seconds);

    timestamp_seconds
        .saturating_add(utc_offset_seconds)
        .div_euclid(SECONDS_PER_DAY)
}

/// Resolves local UTC offset seconds for one Unix timestamp.
fn local_utc_offset_seconds(timestamp_seconds: i64) -> i64 {
    let Ok(utc_timestamp) = OffsetDateTime::from_unix_timestamp(timestamp_seconds) else {
        return 0;
    };
    let Ok(local_offset) = UtcOffset::local_offset_at(utc_timestamp) else {
        return 0;
    };

    i64::from(local_offset.whole_seconds())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::domain::session::SESSION_DATA_DIR;

    /// Ensures reload keeps live handle output and active status when
    /// persisted row data is stale.
    #[tokio::test]
    async fn test_load_sessions_preserves_live_handle_output_and_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.append_session_output(session_id, "DB Output")
            .await
            .expect("failed to append persisted output");

        let session_dir = dir.path().join(session_id);
        std::fs::create_dir_all(session_dir.join(SESSION_DATA_DIR))
            .expect("failed to create session data dir");

        let mut handles = HashMap::new();
        let live_output = "Live Output".to_string();
        let live_status = Status::Review;
        handles.insert(
            session_id.to_string(),
            SessionHandles::new(live_output.clone(), live_status),
        );

        // Act
        let (sessions, _) = SessionManager::load_sessions(
            dir.path(),
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.output, live_output);
        assert_eq!(session.status, live_status);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_output = handle
            .output
            .lock()
            .expect("failed to lock handle output")
            .clone();
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_output, live_output);
        assert_eq!(handle_status, live_status);
    }

    /// Ensures terminal persisted statuses replace stale active handle status
    /// during reload.
    #[tokio::test]
    async fn test_load_sessions_terminal_db_status_overrides_handle_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert session");

        let session_dir = dir.path().join(session_id);
        std::fs::create_dir_all(session_dir.join(SESSION_DATA_DIR))
            .expect("failed to create session data dir");

        let mut handles = HashMap::new();
        handles.insert(
            session_id.to_string(),
            SessionHandles::new("output".to_string(), Status::Review),
        );

        // Act
        let (sessions, _) = SessionManager::load_sessions(
            dir.path(),
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.status, Status::Done);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_status, Status::Done);
    }
}
