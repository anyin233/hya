CREATE TABLE pending_interaction (
    id TEXT PRIMARY KEY,
    session_id BLOB,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE INDEX pending_interaction_live ON pending_interaction(created_at, id) WHERE resolved_at IS NULL;
