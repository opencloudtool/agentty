//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::HashMap;
use std::path::Path;

use super::session_folder;
use crate::domain::agent::{AgentKind, AgentModel};
use crate::app::SessionManager;
use crate::infra::db::Database;
use crate::infra::git;
use crate::domain::project::Project;
use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats, Status};
use crate::model::DailyActivity;

impl SessionManager {
    /// Loads session models from the database and reuses live handles when
    /// possible.
    ///
    /// Existing handles are updated in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Returns both loaded sessions and aggregated daily activity counts.
    pub(crate) async fn load_sessions(
        base: &Path,
        db: &Database,
        projects: &[Project],
        handles: &mut HashMap<String, SessionHandles>,
    ) -> (Vec<Session>, Vec<DailyActivity>) {
        const SECONDS_PER_DAY: i64 = 86_400;

        let project_names: HashMap<i64, String> = projects
            .iter()
            .filter_map(|project| {
                let name = project.path.file_name()?.to_string_lossy().to_string();
                Some((project.id, name))
            })
            .collect();

        let db_rows = db.load_sessions().await.unwrap_or_default();
        let mut activity_by_day: HashMap<i64, u32> = HashMap::new();
        let mut sessions: Vec<Session> = Vec::new();

        for row in db_rows {
            let folder = session_folder(base, &row.id);
            let status = row.status.parse::<Status>().unwrap_or(Status::Done);
            let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
            let is_terminal_status = matches!(status, Status::Done | Status::Canceled);
            if !folder.is_dir() && !is_terminal_status {
                continue;
            }

            let session_model = row
                .model
                .parse::<AgentModel>()
                .unwrap_or_else(|_| AgentKind::Gemini.default_model());
            let project_name = row
                .project_id
                .and_then(|id| project_names.get(&id))
                .cloned()
                .unwrap_or_default();

            if let Some(existing) = handles.get(&row.id) {
                if let Ok(mut output_buffer) = existing.output.lock() {
                    output_buffer.clone_from(&row.output);
                }
                if let Ok(mut status_value) = existing.status.lock() {
                    *status_value = status;
                }
            } else {
                handles.insert(
                    row.id.clone(),
                    SessionHandles::new(row.output.clone(), status),
                );
            }

            let size = if is_terminal_status {
                persisted_size
            } else {
                let computed_size = Self::session_size_for_folder(&folder, &row.base_branch).await;
                let _ = db
                    .update_session_size(&row.id, &computed_size.to_string())
                    .await;

                computed_size
            };

            let created_day_key = row.created_at.div_euclid(SECONDS_PER_DAY);
            let day_entry = activity_by_day.entry(created_day_key).or_insert(0);
            *day_entry = day_entry.saturating_add(1);

            sessions.push(Session {
                base_branch: row.base_branch,
                folder,
                id: row.id,
                model: session_model,
                output: row.output,
                permission_mode: row.permission_mode.parse().unwrap_or_default(),
                project_name,
                prompt: row.prompt,
                size,
                stats: SessionStats {
                    input_tokens: row.input_tokens.cast_unsigned(),
                    output_tokens: row.output_tokens.cast_unsigned(),
                },
                status,
                summary: row.summary,
                title: row.title,
            });
        }

        let mut stats_activity: Vec<DailyActivity> = activity_by_day
            .into_iter()
            .map(|(day_key, session_count)| DailyActivity {
                day_key,
                session_count,
            })
            .collect();
        stats_activity.sort_by_key(|activity| activity.day_key);

        (sessions, stats_activity)
    }

    async fn session_size_for_folder(folder: &Path, base_branch: &str) -> SessionSize {
        if !folder.is_dir() {
            return SessionSize::Xs;
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let diff = git::diff(folder, base_branch)
            .await
            .ok()
            .unwrap_or_default();

        SessionSize::from_diff(&diff)
    }
}
