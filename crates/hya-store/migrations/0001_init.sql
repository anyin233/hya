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
    FOREIGN KEY (parent_id) REFERENCES session(id)
);
CREATE INDEX session_parent ON session(parent_id);

CREATE TABLE message (
    id           BLOB PRIMARY KEY,
    session_id   BLOB NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    role         TEXT NOT NULL,
    agent        TEXT,
    model        TEXT,
    finish       TEXT,
    cost_json    TEXT,
    tokens_json  TEXT,
    created_at   INTEGER NOT NULL
);
CREATE INDEX message_session ON message(session_id);

CREATE TABLE part (
    id           BLOB PRIMARY KEY,
    message_id   BLOB NOT NULL REFERENCES message(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    body_json    TEXT NOT NULL,
    UNIQUE(message_id, seq)
);

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB NOT NULL,
    payload      TEXT NOT NULL,
    ts           INTEGER NOT NULL
);
CREATE INDEX event_log_session ON event_log(session_id);

CREATE TABLE team_run (
    id           BLOB PRIMARY KEY,
    lead_session BLOB NOT NULL REFERENCES session(id),
    spec_json    TEXT NOT NULL,
    state        TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE team_member (
    id                 BLOB PRIMARY KEY,
    team_id            BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    session_id         BLOB NOT NULL REFERENCES session(id),
    background_task_id TEXT,
    role               TEXT NOT NULL,
    state              TEXT NOT NULL,
    created_at         INTEGER NOT NULL
);

CREATE TABLE mail (
    id           BLOB PRIMARY KEY,
    team_id      BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    from_ep      TEXT NOT NULL,
    to_ep        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    body_json    TEXT NOT NULL,
    delivered_at INTEGER,
    acked_at     INTEGER,
    created_at   INTEGER NOT NULL
);

CREATE TABLE task_board (
    id           BLOB PRIMARY KEY,
    team_id      BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    status       TEXT NOT NULL,
    assignee     TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE goal (
    id              BLOB PRIMARY KEY,
    session_id      BLOB NOT NULL REFERENCES session(id),
    condition       TEXT NOT NULL,
    bound_json      TEXT,
    state           TEXT NOT NULL,
    turns_evaluated INTEGER NOT NULL,
    last_reason     TEXT,
    started_at      INTEGER NOT NULL,
    cleared_at      INTEGER
);

CREATE TABLE token_ledger (
    id                BLOB PRIMARY KEY,
    session_id        BLOB NOT NULL,
    turn              INTEGER,
    provider          TEXT,
    model             TEXT,
    team_id           BLOB,
    completion_run_id BLOB,
    iteration         INTEGER,
    role              TEXT,
    category          TEXT,
    prompt_tokens     INTEGER,
    completion_tokens INTEGER,
    confidence        TEXT NOT NULL,
    ts                INTEGER NOT NULL
);
