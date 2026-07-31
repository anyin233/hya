CREATE TABLE resident_actor_claim (
    actor_id     BLOB PRIMARY KEY,
    epoch        INTEGER NOT NULL CHECK (epoch > 0),
    owner_run_id BLOB NOT NULL CHECK (length(owner_run_id) = 16),
    state        TEXT NOT NULL CHECK (state IN ('active', 'released'))
);

CREATE INDEX resident_actor_claim_state
    ON resident_actor_claim(state, actor_id);

ALTER TABLE admission_journal
    ADD COLUMN actor_id BLOB;

ALTER TABLE admission_journal
    ADD COLUMN actor_epoch INTEGER CHECK (actor_epoch IS NULL OR actor_epoch > 0);

CREATE INDEX admission_journal_actor_state
    ON admission_journal(actor_id, actor_epoch, state);
