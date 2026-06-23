CREATE TABLE sync_event (
    aggregate_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (aggregate_id, seq)
);

CREATE INDEX sync_event_seq ON sync_event(seq);
