-- Durable projection snapshots: a pure, replay-consistent cache of the shared
-- reducer (`hya_proto::Projection`), keyed by session and the last folded
-- event seq. Never a source of truth: a row is trusted only when its
-- `reducer_version` equals the running reducer's and its anchor event
-- (`event_log.seq = last_seq` for the same session) still exists; otherwise
-- the read folds the full log and overwrites the row. Starts empty — the
-- first read of each session after this migration folds its log once.
CREATE TABLE projection_snapshot (
    session_id      BLOB PRIMARY KEY,
    reducer_version INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL,
    payload         BLOB NOT NULL
);
