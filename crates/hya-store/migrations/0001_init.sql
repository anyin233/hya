CREATE TABLE session (
    id           BLOB PRIMARY KEY,
    parent_id    BLOB,
    agent        TEXT NOT NULL,
    model        TEXT NOT NULL,
    workdir      TEXT NOT NULL,
    title        TEXT,
    permission   TEXT NOT NULL DEFAULT '{}',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    project_id   TEXT,
    kind         TEXT NOT NULL DEFAULT 'project' CHECK (kind IN ('project', 'temporary')),
    FOREIGN KEY (parent_id) REFERENCES session(id)
);
CREATE INDEX session_parent ON session(parent_id);

CREATE INDEX session_project ON session(project_id);

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB NOT NULL,
    payload      TEXT NOT NULL,
    ts           INTEGER NOT NULL
);
CREATE INDEX event_log_session ON event_log(session_id);

CREATE TABLE saved_permission (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    action     TEXT NOT NULL,
    resource   TEXT NOT NULL,
    time_created INTEGER,
    UNIQUE(project_id, action, resource)
);

CREATE INDEX saved_permission_project ON saved_permission(project_id);

CREATE TABLE admission_journal (
    operation_id        BLOB PRIMARY KEY CHECK (length(operation_id) = 16),
    source_tool_call_id BLOB NOT NULL UNIQUE CHECK (length(source_tool_call_id) = 16),
    root_session_id     BLOB NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    state               TEXT NOT NULL CHECK (
        state IN ('accepted', 'started', 'completed', 'cancelled', 'aborted')
    ),
    admission_units     INTEGER NOT NULL CHECK (admission_units > 0),
    logical_released    INTEGER NOT NULL DEFAULT 0 CHECK (logical_released IN (0, 1)),
    terminal_reason     TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    actor_id            BLOB,
    actor_epoch         INTEGER CHECK (actor_epoch IS NULL OR actor_epoch > 0)
);

CREATE INDEX admission_journal_root_state
    ON admission_journal(root_session_id, state);

CREATE INDEX admission_journal_actor_state
    ON admission_journal(actor_id, actor_epoch, state);

CREATE TABLE resident_actor_claim (
    actor_id     BLOB PRIMARY KEY,
    epoch        INTEGER NOT NULL CHECK (epoch > 0),
    owner_run_id BLOB NOT NULL CHECK (length(owner_run_id) = 16),
    state        TEXT NOT NULL CHECK (state IN ('active', 'released'))
);

CREATE INDEX resident_actor_claim_state
    ON resident_actor_claim(state, actor_id);

CREATE TABLE agent_model_preference (
    agent_id    TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    CHECK(length(agent_id) BETWEEN 1 AND 1024),
    CHECK(length(provider_id) BETWEEN 1 AND 1024),
    CHECK(length(model_id) BETWEEN 1 AND 4096)
);

-- Write-through index of assistant messages that have started but not
-- finished (or been deleted). Maintained inside the event-append transaction
-- (`materialize.rs`); read only by startup crash recovery, so recovery touches
-- only sessions a dead process left mid-turn instead of replaying every log.
CREATE TABLE open_assistant_message (
    session_id BLOB NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (session_id, message_id)
) WITHOUT ROWID;

-- Durable projection snapshots: a pure, replay-consistent cache of the shared
-- reducer (`hya_proto::Projection`), keyed by session and the last folded
-- event seq. Never a source of truth: a row is trusted only when its
-- `reducer_version` equals the running reducer's and its anchor event
-- (`event_log.seq = last_seq` for the same session) still exists; otherwise
-- the read folds the full log and overwrites the row. Starts empty — the
-- first read of an existing log without a snapshot folds it once.
CREATE TABLE projection_snapshot (
    session_id      BLOB PRIMARY KEY,
    reducer_version INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL,
    payload         BLOB NOT NULL
);

-- Content-addressed file contents kept for session revert (`FilesChanged`,
-- `SessionReverted`). Scoped per session so `delete_session` removes them
-- and a session's total is cheap to cap. `hash` is the lowercase hex sha256
-- of `content`; events carry only the hash.
CREATE TABLE file_blob (
    session_id BLOB NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    content    BLOB NOT NULL,
    PRIMARY KEY (session_id, hash)
) WITHOUT ROWID;

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

-- Durable per-model reasoning effort preference. The runtime owner fences writes;
-- rows are scoped to this database and keyed by exact provider/model identity.
CREATE TABLE model_effort_preference (
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    effort      TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);

-- One restore prompt for each session interrupted by a graceful process stop.
CREATE TABLE pending_resume (
    session_id BLOB PRIMARY KEY,
    prompt TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE pending_interaction (
    id TEXT PRIMARY KEY,
    session_id BLOB,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE INDEX pending_interaction_live ON pending_interaction(created_at, id) WHERE resolved_at IS NULL;

-- Durable per-Agent default thinking effort chosen by the user at runtime.
-- Independent of the Agent's model tiers; the runtime owner fences writes.
CREATE TABLE agent_effort_preference (
    agent_id   TEXT NOT NULL PRIMARY KEY,
    effort     TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
