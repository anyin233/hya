-- Projects (ADR-0024): a named, ordered, non-empty set of absolute workspace
-- roots on the backend machine. Mutable configuration managed with CRUD like
-- `saved_permission`, never event-sourced; sessions reference a Project by
-- id in their `session_created` event, mirrored below in `session`.
CREATE TABLE project (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL CHECK (length(trim(name)) > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived   INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))
);

-- `ord = 0` is the primary root (a Project session's default workdir).
CREATE TABLE project_root (
    project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    path       TEXT NOT NULL CHECK (length(path) > 0),
    ord        INTEGER NOT NULL CHECK (ord >= 0),
    PRIMARY KEY (project_id, ord),
    UNIQUE (project_id, path)
);

-- Session membership, written by the `session_created` materializer. No FK:
-- the event log keeps the id even after its Project is deleted. Sessions from
-- before this migration have no Project and are `project` sessions.
ALTER TABLE session ADD COLUMN project_id TEXT;
ALTER TABLE session ADD COLUMN kind TEXT NOT NULL DEFAULT 'project'
    CHECK (kind IN ('project', 'temporary'));
CREATE INDEX session_project ON session(project_id);
