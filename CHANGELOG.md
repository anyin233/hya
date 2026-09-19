# 0.36.48

## The agent-model preference mechanism is double-checked and proven end to end (e2e)

- Code-path audit of the durable per-Agent model preference, confirmed
  against source: the control publishes an immutable snapshot into the
  runtime registry; turn bindings capture it at bind time; the
  preference applies only to agents whose catalog definition has neither
  a direct model nor a category policy (reasoning-only does not
  suppress), and only when the exact model resolves in the current
  provider catalog. Session-tree overrides (`SessionAgentModelOverrideSet`,
  event-sourced on the lineage root) take precedence via the binding's
  overlay; configured policy next; the remembered preference is the
  lowest-precedence default. It steers spawned subagent member specs,
  the summarizer/compaction model, and session-title generation — the
  interactive root turn keeps using the session model by design.
- New e2e scenario T2.16 (`p21_agent_model_preference.rs`): register an
  extra fake model, `PUT /v1/agent-models/general` to it, spawn a
  `general` subagent through the task tool, and assert the recorded
  provider request ran on the preferred model while the listing
  reports the `AGENT_MODEL_SOURCE_REMEMBERED` tier (default tier
  asserted before the set). The e2e harness gains a `put_json` helper;
  the matrix registers the scenario (45 scenarios, 9 retired).
