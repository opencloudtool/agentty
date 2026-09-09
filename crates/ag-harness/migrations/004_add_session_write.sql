CREATE TABLE session_write (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    turn_position INTEGER NOT NULL,
    call_id TEXT NOT NULL,
    repository_root TEXT NOT NULL,
    path TEXT NOT NULL,
    expected_hash TEXT,
    resulting_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'applied', 'failed')),
    FOREIGN KEY (session_id, turn_position)
        REFERENCES session_turn(session_id, turn_position) ON DELETE CASCADE
);

CREATE INDEX session_write_session_turn_idx
ON session_write (session_id, turn_position, id);
