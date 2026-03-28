CREATE TABLE session_follow_up_task (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    text TEXT NOT NULL
);

CREATE UNIQUE INDEX session_follow_up_task_session_id_position_idx
ON session_follow_up_task (session_id, position);

CREATE INDEX session_follow_up_task_session_id_idx
ON session_follow_up_task (session_id);
