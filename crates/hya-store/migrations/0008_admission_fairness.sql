ALTER TABLE admission_journal
    ADD COLUMN admission_sequence INTEGER
        CHECK (admission_sequence IS NULL OR admission_sequence > 0);

ALTER TABLE admission_journal
    ADD COLUMN promotion_sequence INTEGER
        CHECK (promotion_sequence IS NULL OR promotion_sequence > 0);

UPDATE admission_journal
SET admission_sequence = rowid
WHERE admission_sequence IS NULL;

CREATE UNIQUE INDEX admission_journal_admission_sequence
    ON admission_journal(admission_sequence)
    WHERE admission_sequence IS NOT NULL;

CREATE UNIQUE INDEX admission_journal_promotion_sequence
    ON admission_journal(promotion_sequence)
    WHERE promotion_sequence IS NOT NULL;

CREATE INDEX admission_journal_queued_admission_sequence
    ON admission_journal(state, admission_sequence)
    WHERE state = 'queued' AND admission_sequence IS NOT NULL;

CREATE INDEX admission_journal_root_promotion_sequence
    ON admission_journal(root_session_id, promotion_sequence)
    WHERE promotion_sequence IS NOT NULL;
