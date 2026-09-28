-- Hot-reload handoff (`hya serve restart` invoked inside a shell turn): the runtime
-- owner checkpoints a session by closing its open turn (finish cause
-- `handoff`) and inserting one row here in the same transaction. The process
-- that drives the continuation acknowledges the row exactly once
-- (`begin_handoff_resume` sets `taken_at` and appends the durable
-- continuation-start marker in the same transaction); taken rows are kept as
-- durable evidence so a successor that restarts can never be resumed twice.
CREATE TABLE pending_resume (
    id         TEXT PRIMARY KEY,
    session_id BLOB NOT NULL,
    owner_run  BLOB NOT NULL CHECK (length(owner_run) = 16),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    prompt     TEXT NOT NULL,
    taken_by   BLOB CHECK (taken_by IS NULL OR length(taken_by) = 16),
    taken_at   INTEGER,
    created_at INTEGER NOT NULL,
    CHECK ((taken_by IS NULL) = (taken_at IS NULL))
);

-- At most one live (untaken) resume per session: a second checkpoint for the
-- same session is refused instead of queueing a duplicate successor resume.
CREATE UNIQUE INDEX pending_resume_live_session
    ON pending_resume(session_id) WHERE taken_at IS NULL;

-- Successor startup drains live resumes oldest generation first.
CREATE INDEX pending_resume_live_order
    ON pending_resume(generation, created_at, id) WHERE taken_at IS NULL;
