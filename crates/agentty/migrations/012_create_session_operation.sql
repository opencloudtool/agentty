CREATE TABLE session_operation (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    heartbeat_at INTEGER,
    last_error TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX session_operation_session_id_status_idx
ON session_operation (session_id, status);
