CREATE INDEX IF NOT EXISTS idx_session_project_updated_at
ON session (project_id, updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_session_updated_at
ON session (updated_at DESC, id);

CREATE INDEX IF NOT EXISTS idx_session_activity_created_at
ON session_activity (created_at);
