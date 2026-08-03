ALTER TABLE admission_journal
    ADD COLUMN runtime_fingerprint_version INTEGER
        CHECK (
            runtime_fingerprint_version IS NULL
            OR (runtime_fingerprint_version >= 0 AND runtime_fingerprint_version <= 4294967295)
        );

ALTER TABLE admission_journal
    ADD COLUMN runtime_fingerprint BLOB
        CHECK (runtime_fingerprint IS NULL OR length(runtime_fingerprint) = 32);

ALTER TABLE admission_journal
    ADD COLUMN admission_binding_fingerprint_version INTEGER
        CHECK (
            admission_binding_fingerprint_version IS NULL
            OR (
                admission_binding_fingerprint_version >= 0
                AND admission_binding_fingerprint_version <= 4294967295
            )
        );

ALTER TABLE admission_journal
    ADD COLUMN admission_binding_fingerprint BLOB
        CHECK (
            admission_binding_fingerprint IS NULL
            OR length(admission_binding_fingerprint) = 32
        );

ALTER TABLE admission_journal
    ADD COLUMN spawn_intent BLOB
        CHECK (
            spawn_intent IS NULL
            OR (length(spawn_intent) >= 1 AND length(spawn_intent) <= 1048576)
        )
        CHECK (
            (
                runtime_fingerprint_version IS NULL
                AND runtime_fingerprint IS NULL
                AND admission_binding_fingerprint_version IS NULL
                AND admission_binding_fingerprint IS NULL
                AND spawn_intent IS NULL
            )
            OR (
                runtime_fingerprint_version IS NOT NULL
                AND runtime_fingerprint IS NOT NULL
                AND admission_binding_fingerprint_version IS NOT NULL
                AND admission_binding_fingerprint IS NOT NULL
                AND spawn_intent IS NOT NULL
            )
        );
