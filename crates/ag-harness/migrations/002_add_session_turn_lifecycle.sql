ALTER TABLE session ADD COLUMN provider_session_id TEXT;

CREATE TABLE session_turn (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_position INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('pending', 'running', 'completed', 'failed', 'interrupted')
    ),
    error_type TEXT,
    lease_expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_position),
    CHECK (
        (status IN ('pending', 'running') AND lease_expires_at IS NOT NULL)
        OR (status NOT IN ('pending', 'running') AND lease_expires_at IS NULL)
    )
);

INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
SELECT
    session_id,
    turn_position,
    'completed',
    NULL,
    NULL,
    MIN(created_at),
    MAX(created_at)
FROM session_message
GROUP BY session_id, turn_position;

CREATE UNIQUE INDEX session_turn_one_active_idx
ON session_turn (session_id)
WHERE status IN ('pending', 'running');
