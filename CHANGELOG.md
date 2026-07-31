# 0.34.4

- Derive an internal, domain-separated operation identity from each persisted
  tool call and carry it through task spawning without adding a public API.
- Add a narrow SQLite admission journal with immutable request fingerprints,
  idempotent state transitions, typed operation conflicts, and fail-closed
  startup recovery.
- Converge transient and resident spawn admission, cancellation, completion,
  overload, and root cleanup on one exactly-once debit/finalize path.
