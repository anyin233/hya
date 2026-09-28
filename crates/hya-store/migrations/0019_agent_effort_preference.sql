-- Durable per-Agent default thinking effort chosen by the user at runtime.
-- Independent of the Agent's model tiers; the runtime owner fences writes.
CREATE TABLE agent_effort_preference (
    agent_id   TEXT NOT NULL PRIMARY KEY,
    effort     TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
