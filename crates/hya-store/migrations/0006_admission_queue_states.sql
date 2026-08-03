CREATE TABLE admission_journal_queue_states (
    operation_id        BLOB NOT NULL CHECK (length(operation_id) = 16),
    source_tool_call_id BLOB NOT NULL CHECK (length(source_tool_call_id) = 16),
    root_session_id     BLOB NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    state               TEXT NOT NULL CHECK (
        state IN ('queued', 'accepted', 'started', 'waiting', 'completed', 'cancelled', 'aborted')
    ),
    admission_units     INTEGER NOT NULL CHECK (admission_units > 0),
    logical_released    INTEGER NOT NULL DEFAULT 0 CHECK (logical_released IN (0, 1)),
    terminal_reason     TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    actor_id            BLOB,
    actor_epoch         INTEGER CHECK (actor_epoch IS NULL OR actor_epoch > 0),
    member_ordinal      INTEGER NOT NULL DEFAULT 0 CHECK (member_ordinal >= 0),
    batch_size          INTEGER NOT NULL DEFAULT 1 CHECK (batch_size > 0),
    CHECK (member_ordinal < batch_size),
    PRIMARY KEY (operation_id, member_ordinal),
    UNIQUE (source_tool_call_id, member_ordinal)
);

INSERT INTO admission_journal_queue_states (
    operation_id,
    source_tool_call_id,
    root_session_id,
    request_fingerprint,
    state,
    admission_units,
    logical_released,
    terminal_reason,
    created_at,
    updated_at,
    actor_id,
    actor_epoch,
    member_ordinal,
    batch_size
)
SELECT
    operation_id,
    source_tool_call_id,
    root_session_id,
    request_fingerprint,
    state,
    admission_units,
    logical_released,
    terminal_reason,
    created_at,
    updated_at,
    actor_id,
    actor_epoch,
    0,
    1
FROM admission_journal;

DROP TABLE admission_journal;

ALTER TABLE admission_journal_queue_states RENAME TO admission_journal;

CREATE INDEX admission_journal_root_state
    ON admission_journal(root_session_id, state);

CREATE INDEX admission_journal_actor_state
    ON admission_journal(actor_id, actor_epoch, state);
