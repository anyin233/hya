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
    updated_at          INTEGER NOT NULL
);

CREATE INDEX admission_journal_root_state
    ON admission_journal(root_session_id, state);
