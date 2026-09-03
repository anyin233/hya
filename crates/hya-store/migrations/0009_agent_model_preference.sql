CREATE TABLE agent_model_preference (
    agent_id    TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    CHECK(length(agent_id) BETWEEN 1 AND 1024),
    CHECK(length(provider_id) BETWEEN 1 AND 1024),
    CHECK(length(model_id) BETWEEN 1 AND 4096)
);
