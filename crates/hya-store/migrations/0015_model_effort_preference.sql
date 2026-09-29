-- Durable per-model reasoning effort preference. The runtime owner fences writes;
-- rows are scoped to this database and keyed by exact provider/model identity.
CREATE TABLE model_effort_preference (
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    effort      TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);
