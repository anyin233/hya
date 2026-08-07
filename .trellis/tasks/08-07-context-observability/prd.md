# Context observability: recoverable call graph and compaction history

## Goal

Make the event store a **sufficient statistic** for offline reconstruction of:

1. the agent call graph of a session (all agents, all edges),
2. each agent's full trajectory,
3. every compaction — when it ran, what range it folded, and what it produced.

An offline viewer reading only the SQLite store must rebuild all three without
guessing. This task records data; it does not build the viewer, and it does not
change what any model sees except where explicitly stated in R6.

## Background

Audit of the current store found the substrate largely in place:

- Every subagent already owns its `SessionId`, its `session` row, and its
  `event_log` rows. Trajectories are already isolated per agent.
- Compaction never deletes. It appends a `HYA_COMPACTED_CONTEXT` System marker;
  `compacted_messages()` (`crates/hya-core/src/engine/turn/messages.rs:85`)
  slices only the *model input*. The log stays complete.
- `event_log.seq` is a single `AUTOINCREMENT` across all sessions, so parent and
  child events already interleave on one global causal timeline.
- Spawn edges exist as `MemberSpawned` / `MemberFinished`; lateral edges exist as
  `MailSent` / `ChannelJoined` / `ChannelLeft`; `build_run_tree`
  (`crates/hya-proto/src/projection_tree.rs`) already assembles a spawn tree.

Five gaps block offline reconstruction. They are the scope of this task.

## Requirements

### R1 — Compaction is fully visible

A new additive `Event::ContextCompacted` records every compaction on the log of
the session that compacted, carrying: the output marker message id, the strategy
used (native provider compact vs local summarizer), the folded range as
`MessageId` endpoints, the folded message count, the token estimate that tripped
the threshold, and the threshold in force.

The event stores a **pointer only**. It must not embed the rendered summarizer
input; the folded range plus the never-deleted log reconstructs that exactly.

### R2 — Local compaction output is durable

The local-summarizer path currently discards its result: `compact_with` returns a
`Vec` used for one request only (`crates/hya-core/src/engine/turn.rs:619`). It
must instead inject its summary through the same `HYA_COMPACTED_CONTEXT` marker
path the native provider compact already uses.

This is the one requirement that changes runtime behavior. It is accepted
deliberately: it makes the local summary recoverable **and** stops the engine
re-summarizing the same history on every round.

### R3 — Subagent purpose is recorded verbatim on the spawn edge

`MemberSpawned` gains the parent's full directive. Today the directive is only
recoverable as the child's *first user message*, which is wrong for resumed
sessions (the directive lands as a later message) and for resident agents (mail
also arrives as user prompts).

The directive is stored verbatim and unbounded. Summarizing it into a "purpose"
is the offline viewer's job, not the engine's.

### R4 — Spawn edges anchor to their originating tool call

`MemberSpawned` gains the `ToolCallId` of the `task` call that caused it, so the
graph can anchor each edge to an exact point in the parent's trajectory instead
of inferring it from `seq` proximity.

### R5 — Forks are not orphans

A forked session currently records no provenance at all: `session_fork.rs:33`
sets `parent: None`, `copy_messages_to_session` re-emits every message with a new
`MessageId`, and the `before` cut point is discarded. The only surviving trace is
a `" (fork #N)"` title suffix.

A fork must record its source session and its cut point so the graph can render a
fork edge.

### R6 — No summary reaches any model context

Everything recorded here is view-only. No new material may be added to any
parent's or any agent's model input. R2 is the sole permitted change to model
input, and only for the session that compacts.

### R7 — Backward and forward compatible

- No new tables and no migrations. All new data rides existing `event_log` rows.
- Every `Event` change is additive and uses `#[serde(default)]`, matching the
  existing precedent on `MemberSpawned.agent_type` and `.mode`.
- Logs written before this task must still replay without error.
- Binaries older than this task must fold the new variant via `Event::Unknown`.

### R8 — Reconstruction stays derived

The graph is assembled at read time from replay. Do not add a maintained
read-model table — `projection_tree.rs` warns against exactly that drift. This
task only guarantees the inputs exist.

## Constraints

- `hya-proto` stays dependency-light (`AGENTS.md` Change Guidance).
- Library crates deny `unwrap_used` / `expect_used`.
- Preserve the event-sourced architecture: append events, replay with the shared
  projection, add no parallel read-model logic.
- Retention is unbounded; subagents may run long and nothing is pruned.

## Acceptance Criteria

- [ ] AC1 — A compaction on any session appends exactly one `ContextCompacted`
      to that session's log, carrying strategy, `from_message`, `to_message`,
      `folded_count`, `input_tokens_est`, `threshold`, and the output `message`.
- [ ] AC2 — Both compaction strategies emit `ContextCompacted`: the native
      provider path and the local summarizer path.
- [ ] AC3 — After a local-summarizer compaction, the summary is present in the
      session log behind a `HYA_COMPACTED_CONTEXT` marker, and the next round
      slices from that marker instead of re-summarizing. Proven by a test that
      counts summarizer invocations across two rounds.
- [ ] AC4 — `ContextCompacted.from_message`/`to_message` resolve to messages that
      are still present in the log, and the range covers exactly
      `folded_count` messages.
- [ ] AC5 — A spawned subagent's `MemberSpawned` carries the parent's directive
      verbatim, for a fresh spawn and for a resumed session.
- [ ] AC6 — `MemberSpawned` carries the `ToolCallId` of the originating `task`
      call, and it matches a tool part present in the parent's trajectory.
- [ ] AC7 — A forked session is reachable from its source: the fork's source
      session id and cut point are recoverable from the log.
- [ ] AC8 — A log containing every new event replays cleanly on the shared
      projection, and a log written before this task still replays unchanged.
- [ ] AC9 — Parent model input is byte-identical before and after this task for
      a run containing spawns and child compactions. Proven by a test asserting
      no new material entered the parent request.
- [ ] AC10 — `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
      -- -D warnings`, and `cargo test --workspace --exclude hya-e2e` pass.
- [ ] AC11 — No migration file is added and no table is created.

## Non-Goals

- The offline visualizer. That is the next task.
- Deriving "purpose" from the directive. The viewer does that.
- Any context-efficiency change: `max_context`-relative thresholds, real token
  counts from `token_ledger`, selective eviction, cross-agent `AGENTS.md`
  sharing. Those belong to the sibling task `08-07-context-efficiency`, which
  depends on the numbers R1 makes available.
- Any read API or export CLI.

## Open Risks

- **R2 changes model input on non-native routes.** Sticky local compaction means
  later rounds slice from the marker. Intended, but it needs a dedicated
  regression test, and it is the only item in this task that can change agent
  behavior.
- **R5 may need a decision** on whether to reuse `SessionCreated.parent` for the
  fork link or add a distinct event. Reusing `parent` would make forks look like
  subagent children in the spawn tree, which is wrong — the tree derives from
  spawn edges only. `design.md` resolves this.
