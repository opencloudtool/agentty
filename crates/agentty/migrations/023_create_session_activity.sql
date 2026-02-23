CREATE TABLE session_activity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT UNIQUE REFERENCES session(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX session_activity_created_at_idx ON session_activity (created_at);
CREATE INDEX session_activity_session_id_idx ON session_activity (session_id);

INSERT INTO session_activity (session_id, created_at)
SELECT id, created_at
FROM session;
