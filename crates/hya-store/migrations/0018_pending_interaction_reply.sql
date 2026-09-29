-- Durable answers submitted after the owning in-memory oneshot disappeared
-- during a daemon handoff. The successor consumes rows exactly once after it
-- appends the corresponding tool result/error and starts continuation.
CREATE TABLE pending_interaction_reply (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    consumed_at INTEGER
);
CREATE INDEX pending_interaction_reply_order
    ON pending_interaction_reply(created_at, id) WHERE consumed_at IS NULL;
