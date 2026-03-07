CREATE TABLE session_review_request (
    session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
    display_id TEXT NOT NULL,
    forge_kind TEXT NOT NULL,
    last_refreshed_at INTEGER NOT NULL,
    source_branch TEXT NOT NULL,
    state TEXT NOT NULL,
    status_summary TEXT,
    target_branch TEXT NOT NULL,
    title TEXT NOT NULL,
    web_url TEXT NOT NULL
);
