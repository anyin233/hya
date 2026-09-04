# 0.36.10

## Remember per-Agent model selections

- Add backend-owned, owner-fenced per-Agent model preferences in the active Session SQLite database, with immutable turn snapshots and exact provider-catalog validation.
- Apply remembered defaults to later root Sessions, subagent admissions, unassigned Workflow members, and the hidden Title, Summary, and Compaction Agents while preserving configured and request-scoped routing precedence.
- Add the capability-gated `/tui/agent-models` control API and an `Agent models` TUI flow for primary, subagent, and hidden system Agents; configured rows remain visible but disabled.
- Persist TUI selections immediately, retain stale identities without executing them, and keep recents, favorites, variants, credentials, prompts, and provider responses outside the preference table.
- Add focused persistence, isolation, precedence, restart, server API, and frontend decoding coverage; document database scope and model-resolution order.
