CREATE TABLE bundle_registry_generation (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation INTEGER NOT NULL CHECK (generation >= 0)
);

INSERT INTO bundle_registry_generation (singleton, generation) VALUES (1, 0);

CREATE TABLE installed_bundle (
    bundle_id       TEXT PRIMARY KEY,
    version         TEXT NOT NULL,
    publisher       TEXT NOT NULL,
    source_digest   BLOB NOT NULL CHECK (length(source_digest) = 32),
    prepared_digest TEXT NOT NULL,
    prepared_bytes  BLOB NOT NULL,
    installed_at    INTEGER NOT NULL
);
