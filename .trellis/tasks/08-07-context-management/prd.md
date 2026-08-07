# Context management: observability and efficiency

Parent task. Owns the requirement set and the cross-child acceptance criteria.
It has no direct implementation work.

## Source requirement

Two goals, agreed in the originating session:

1. **Observability** — log a detailed agent call graph for a session; store each
   subagent's trajectory as a separate record set; keep the main agent's full
   trajectory. An **offline** visualizer will later render the graph and derive
   each subagent's purpose. This task family only guarantees the store holds
   enough to do that.
2. **Efficiency** — fix the context-management gaps found in the audit: the
   threshold ignores the model's real context window, the estimator is
   `chars / 4`, compaction is all-or-nothing, and every subagent re-renders the
   same `AGENTS.md` chain into its own system prompt.

## Settled decisions (apply to all children)

- The graph is **flat** `{nodes[], edges[]}`; the tree derives from **spawn edges
  only**. Mail and fork edges cannot be expressed by nesting.
- The graph is **derived at runtime**. No new tables, no read-model. The only
  obligation is that storage is a sufficient statistic.
- A subagent's **purpose is the parent's directive**, recorded verbatim.
  Summarization happens **offline in the viewer**, never in the engine.
- No summary ever returns to a parent's model context. View-only.
- Compaction is visible: when it ran, the trajectory range it folded, and its
  output.
- Retention is **unbounded**. Subagents may run long; nothing is pruned.
- The visualizer itself is **out of scope** for this family — it is the next work.

## Task map

| Child | Scope | Risk class | Status |
| --- | --- | --- | --- |
| `08-07-context-observability` | C1 `ContextCompacted`, C2 persist local compaction, C3 directive on `MemberSpawned`, C4 tool-call link, C5 fork provenance | Record-only, except C2 | **Completed** 2026-08-07, branch `feat/context-observability`, released as 0.34.15 |
| `08-07-context-efficiency` | `max_context`-relative thresholds, real token counts from `token_ledger`, selective eviction, cross-agent `AGENTS.md` sharing | Changes what the model sees | Planned, not started |

## Ordering

`context-observability` first. `context-efficiency` depends on it: the numbers
C1 records (`input_tokens_est`, `threshold`, `folded_count`, strategy) are what
make the efficiency work measurable rather than guessed.

The split is by **risk class**, not by size. Observability only changes what is
recorded; efficiency changes what the model sees. Mixing them would put two
different verification burdens behind one gate.

## Cross-child acceptance criteria

- [x] P1 — An offline reader with only the SQLite store can rebuild the full call
      graph, every agent trajectory, and every compaction, for a run that
      includes nested subagents, mail, a fork, and at least one compaction.
      *All inputs now recorded; the reader itself is the next task.*
- [x] P2 — No recorded observability material appears in any model's input.
      *`recorded_observability_never_enters_the_parent_model_input`.*
- [x] P3 — No new tables or migrations are introduced by either child.
      *Holds for child A; still binding on child B.*
- [ ] P4 — Compaction decisions are driven by real token counts and the model's
      advertised `max_context`, not by `chars / 4` against a flat constant.
      *Child B. Child A records `input_tokens_est` + `threshold`, which is what
      makes this measurable.*
- [~] P5 — Full gate green after each child. Child A: `cargo test --workspace
      --exclude hya-e2e` 1323 passing, E2E 30 passing, clippy clean on all five
      crates touched. `cargo fmt --all --check` and workspace clippy still fail
      **only inside `crates/hya-sdk`**, which neither child touches and which
      already fails both on `main` (48 clippy errors; its `mod tests` lacks the
      `allow` attribute). Fixing it belongs to whoever owns the in-flight
      `hya-sdk` work, not to this tree.

## Audit findings that motivated the split

Already in place, not to be rebuilt:

- Per-subagent trajectory isolation — each subagent owns its `SessionId`,
  `session` row, and `event_log` rows.
- Main-agent full trajectory — compaction never deletes; it injects a
  `HYA_COMPACTED_CONTEXT` marker and only the *model input* is sliced.
- Global cross-agent causal order — `event_log.seq` is one `AUTOINCREMENT`
  across all sessions.
- Spawn edges (`MemberSpawned` / `MemberFinished`), lateral edges (`MailSent`,
  `ChannelJoined` / `ChannelLeft`), and a spawn-tree assembler
  (`crates/hya-proto/src/projection_tree.rs`).
