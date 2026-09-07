CREATE TABLE session_preparation (
    session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('preparing', 'ready', 'failed', 'canceled')),
    start_ref TEXT NOT NULL,
    prompt TEXT,
    error TEXT
);
