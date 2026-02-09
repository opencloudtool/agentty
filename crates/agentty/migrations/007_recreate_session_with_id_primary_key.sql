DROP TRIGGER IF EXISTS update_session_insert_timestamps;
DROP TRIGGER IF EXISTS update_session_updated_at;

CREATE TABLE session_new (
    id          TEXT PRIMARY KEY NOT NULL,
    agent       TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'Done',
    project_id  INTEGER REFERENCES project(id),
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0,
    title       TEXT
);

INSERT INTO session_new (id, agent, base_branch, status, project_id, created_at, updated_at, title)
SELECT name, agent, base_branch, status, project_id, created_at, updated_at, title
FROM session;

DROP TABLE session;
ALTER TABLE session_new RENAME TO session;

CREATE TRIGGER update_session_insert_timestamps
AFTER INSERT ON session
WHEN NEW.created_at = 0 OR NEW.updated_at = 0
BEGIN
    UPDATE session
    SET created_at = CASE
            WHEN NEW.created_at = 0 THEN CAST(strftime('%s', 'now') AS INTEGER)
            ELSE NEW.created_at
        END,
        updated_at = CASE
            WHEN NEW.updated_at = 0 THEN CAST(strftime('%s', 'now') AS INTEGER)
            ELSE NEW.updated_at
        END
    WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER update_session_updated_at
AFTER UPDATE ON session
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE session
    SET updated_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE rowid = NEW.rowid;
END;
