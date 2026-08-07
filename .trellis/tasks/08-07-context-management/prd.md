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

| Child | Scope | Risk class |
| --- | --- | --- |
| `08-07-context-observability` | C1 `ContextCompacted`, C2 persist local compaction, C3 directive on `MemberSpawned`, C4 tool-call link, C5 fork provenance | Record-only, except C2 |
| `08-07-context-efficiency` | `max_context`-relative thresholds, real token counts from `token_ledger`, selective eviction, cross-agent `AGENTS.md` sharing | Changes what the model sees |

## Ordering

`context-observability` first. `context-efficiency` depends on it: the numbers
C1 records (`input_tokens_est`, `threshold`, `folded_count`, strategy) are what
make the efficiency work measurable rather than guessed.

The split is by **risk class**, not by size. Observability only changes what is
recorded; efficiency changes what the model sees. Mixing them would put two
different verification burdens behind one gate.

## Cross-child acceptance criteria

- [ ] P1 — An offline reader with only the SQLite store can rebuild the full call
      graph, every agent trajectory, and every compaction, for a run that
      includes nested subagents, mail, a fork, and at least one compaction.
- [ ] P2 — No recorded observability material appears in any model's input.
- [ ] P3 — No new tables or migrations are introduced by either child.
- [ ] P4 — Compaction decisions are driven by real token counts and the model's
      advertised `max_context`, not by `chars / 4` against a flat constant.
- [ ] P5 — Full gate green after each child:
      `cargo fmt --all --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --exclude hya-e2e`.

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
