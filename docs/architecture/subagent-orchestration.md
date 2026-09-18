# Subagent Orchestration

This document is the module-level specification for the unified subagent
orchestration redesign. It is normative for implementation; the decisions and
their rationale live in the ADRs:

- [ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md) — unified
  resident lifecycle (episodic actors), supersedes ADR-0002's hybrid model.
- [ADR-0016](../adr/0016-channel-communication-plane.md) — channel
  communication plane, supersedes ADR-0011.
- [ADR-0017](../adr/0017-workflow-on-unified-substrate.md) — workflow
  execution on the unified substrate.

The redesign is **breaking by mandate**: legacy logs and tool shapes are not
preserved. Compatibility shims are not part of any phase.

Related: [admission-and-governor.md](admission-and-governor.md),
[event-model.md](event-model.md), [agent-tool-surface.md](agent-tool-surface.md).

## 1. Episode model

A subagent is an **episodic actor**. One episode runs from arming to report;
between episodes the agent exists only as a handoff document in the archive
index.

```
spawn (non-blocking; returns handle + DM channel)
  └─ episode n: context = system prompt + handoff(n-1) + arming DM body
       working ⇄ idle          mail on the DM channel wakes (existing resident wake)
       └─ report(result)       gate: inbox drained ∧ all direct children archived
            ├─ handoff(n)      aux model call, state-only, anchors on handoff(n-1)
            └─ ARCHIVE txn     SubagentReported + HandoffCommitted + AgentArchived
                               + claim release + budget refund + roster removal
                               + group-channel membership removal
                               + report mail on the DM channel → wakes parent
revive (downward DM to archived child)
  └─ AgentRestarted + AgentRegistered upsert, epoch bump, budget re-debit,
     arm episode n+1 with context = system prompt + handoff(n) + DM body
```

Termination signals other than a model-issued report:

| Path | Producer | Result |
| --- | --- | --- |
| Turn terminal error | engine | failure report + degraded handoff + archive |
| Budget kill (per-team turn/message) | governor/supervisor | failure report + degraded handoff + archive |
| Parent `kill(child)` | parent tool call | failure report + degraded handoff + archive |
| Root teardown | engine (root turn end) | force-archive every live descendant |
| Handoff model call fails | engine | deterministic degraded handoff (projection-derived), marked `degraded`; archive proceeds |

An idle agent without a report is **never** auto-archived: it may be awaiting
a parent answer. A stuck child surfaces through the parent's report gate,
which rejects reporting while any direct child is live.

Derived invariant (used by the channel plane): **while I am live, my parent is
live** — a parent cannot archive before all its children archive. Upward mail
never resolves to an archived peer; revival is exclusively downward.

## 2. Unified spawn admission

One path replaces both the durable-owner (transient) and legacy (resident)
routes. The admission journal covers **spawn idempotency and budget**, the
actor claim covers the **lifetime**; the journal finalizes when the actor is
registered, not when its work ends.

```
task(members[])                       tool layer: parse/normalize, no execution decisions
→ SpawnerPlane try_send               bounded queue; full ⇒ typed overload
→ supervisor (single loop)
   1 authorize per member             binding.resolve_agent + can_spawn roster
                                      ⇒ unknown_agent_id / agent_spawn_not_allowed
   2 depth check                      depth+1 > MAX_SUBAGENT_DEPTH (2) ⇒ typed reject
   3 SpawnIntentV1 encode + journal batch claim     (queued/accepted …, FIFO promotion)
   4 governor reserve                 per-run budget debit
   5 create session; actor claim (epoch 1)
   6 registration txn                 AgentRegistered{Resident} + MemberSpawned
                                      + group-channel join + DM-channel create
   7 arm episode                      initial directive = episode 1 context
   8 reply handles + DM channel ids   journal → Completed
```

Steps 3–4 reuse the existing journal/governor seam
([admission-and-governor.md](admission-and-governor.md)) with two changes:

- `SpawnAdmissionOutcome` loses nothing; the foreground whole-batch and
  background-on-register reply modes are deleted — every reply is the
  handle-immediately mode.
- `finalize_spawn_admission` fires at registration (step 6 commit), so the
  journal's terminal state no longer tracks task completion.

`per_run_budget` becomes a lease ledger: **refund on archive, debit on spawn
and on revive**. The refund rides the same `logical_released` exactly-once
mechanism; the archive transaction is the sole refund trigger.

Depth is a constant `MAX_SUBAGENT_DEPTH = 2` in `hya-core`
(`crates/hya-core/src/lib.rs` re-export). The `subagent.max_depth` config key
is removed; `SubagentLimits` keeps concurrency/budget fields only.

## 3. Report, handoff, archive

### 3.1 The report gate

`report(result)` is a tool on the communication plane. The engine rejects it
with a typed error unless, atomically under the team lock:

1. `cursor == inbox_len` for the reporting handle (no unread mail), and
2. the handle has zero live direct children (all archived).

The **archive transaction** re-checks both conditions at commit time; a
message that committed in between aborts the archive and wakes a follow-up
turn instead of being lost. There is no timing window by construction.

### 3.2 Handoff pipeline

The handoff call is the existing compaction handoff machinery with a
state-only template: `SummarizeOptions { handoff: true }`, verbatim transcript
plus one trailing prompt, executed by the **subagent's configured model**
(model policy chain unchanged; a dedicated `handoff.model`/`handoff.category`
override may point it at a cheaper tier).

Template (state-only variant of `HANDOFF_TEMPLATE`; "what is", not "how we got
here"):

```
1. Goal - the task, in the parent's terms.
2. Current state - what exists and works right now.
3. Files and code - exact paths touched, and what changed or matters in each.
4. Decisions - choices in force, including what was ruled out.
5. Pending tasks - work explicitly requested and not yet done.
6. Next step - the single next action, or `none`.
```

Each generation anchors on `<previous-handoff>` (the `previous_summary`
mechanism), so handoffs converge rather than accumulate. Episode n's context
includes only handoff(n-1) — never the transcript. In-episode context
compaction (the five-rung ladder) is unchanged and orthogonal; the terminal
handoff supersedes whatever the episode compacted internally.

Degraded handoffs (model call failed, or engine-synthesized terminality) are
derived deterministically from the projection: last assistant message +
roster facts, written under the same six headings where known, and flagged
`degraded: true`. Terminality never blocks on the summarizer.

### 3.3 Storage

Handoff documents are events (`HandoffCommitted`) on the child session log;
the archive index (§7) carries the parsed section digests for search. The
full document is replayable; `compacted_messages`-style truncation does not
apply to it.

## 4. Revival

Trigger: a DM whose recipient handle resolves to an **archived direct child**
of the sender. The write gate's resolution order is:

```
1. live roster ∩ membership+direction rules ⇒ deliver (wake)
2. own-archived direct children (archive index) ⇒ REVIVE
3. anything else ⇒ unknown (indistinguishable rejection)
```

Revive sequence (store-transactional where marked):

1. resolve archived handle → session, stable agent id, latest handoff;
2. re-resolve runtime binding for the stable id (catalog must still contain
   it; fingerprint drift ⇒ typed reject, no silent fallback);
3. actor claim: epoch bump on the same actor id (existing fence);
4. **txn**: `AgentRestarted` + `AgentRegistered` upsert (same canonical
   handle, mode Resident) + DM-channel membership re-check;
5. governor re-debit (revive counts as a new lease);
6. arm episode n+1: context = system prompt + handoff(n) + `[mail from
   parent] <DM body>`; the DM body is the new task.

Concurrent revives of one archived agent: the claim CAS admits exactly one;
the loser receives a typed conflict. Reviving a **live** agent is impossible —
branch 1 delivers the mail normally.

## 5. Channel plane

### 5.1 Kinds and minting

| Kind | Id | Members | Posting | Lifetime |
| --- | --- | --- | --- | --- |
| Group | `#announce-{8}` | leader + direct reports | leader only (`broadcast`) | per unit; membership ends at archive |
| DM | `#DM-{8}` | exactly two (vertical pair) | both (`dm`) | created at spawn (top-down); **persists across archive/revive** |

- 8 random `[a-zA-Z0-9]` chars, collision-checked within the team root's
  channel table, re-minted on collision; minted ids are event facts
  (`ChannelCreated`), so replay is stable without derivation.
- Randomness is naming, not authorization: the write gate checks membership
  and role inside the store transaction.
- Group channels expose **no member list** through any tool. DM channels
  always expose the peer identity (derived from membership, not from input).

### 5.2 Write gate (dm / broadcast / report delivery)

```
dm(to?, body):
  acting identity from session context (never model input)
  child ⇒ to is the parent DM channel (parameter optional/ignored)
  leader ⇒ to must name a direct child (live ⇒ deliver; archived ⇒ revive §4)
  ⇒ MailSent{Channel(#DM-…)}; recipient wake (sender excluded)

broadcast(body):
  caller must lead a unit ⇒ MailSent{Channel(#announce-…)} to live members
  archived members are not members; nothing is queued for them
  caller leads nobody ⇒ typed error (advertised at L2, errors at runtime)

report(result):
  gate §3.1 ⇒ handoff §3.2 ⇒ archive txn §3.3 ⇒ report mail on the DM channel
```

`MailEndpoint::Handle` is deleted; all delivery is `MailEndpoint::Channel`.
Old logs containing handle-addressed mail or named channels are not guaranteed
to fold — accepted breakage.

### 5.3 Wake delivery

Unchanged seam: bus `MailSent` → supervisor `on_mail` → channel members minus
sender → `pending` + notify; busy recipients coalesce into one follow-up turn.
New: membership excludes archived handles at fold time, and a DM whose
recipient is archived routes to the revive path before wake.

## 6. Tool surface and depth policy

| Plane | Tools | Depth 0/1 (main, L1) | Depth 2 (L2) |
| --- | --- | --- | --- |
| Orchestration | `task`, `list_agents`, `workflow`, `search_agent`, `kill` | advertised | **not advertised** |
| Communication | `dm`, `broadcast`, `list_channel`, `report` | advertised | advertised (`broadcast` errors: leads nobody) |
| Coding/etc. | read/write/edit/bash/… | advertised | advertised |

Enforcement is two-layer, engine-owned:

1. **Advertisement filter** keyed on session lineage depth (engine, not
   catalog/resource policy — a bundle must not be able to reintroduce
   orchestration tools at L2).
2. **Admission depth check** (§2 step 2) as the API-level backstop.

Deleted tools: `roster`, `channels`, `join`, `leave` (folded into
`list_channel`). New tools: `dm`, `broadcast`, `list_channel`, `report`,
`search_agent`, `kill`.

`list_channel` output: channel id, kind, can-speak, unread count, and for DM
channels the peer identity; only channels the caller belongs to; archived
peers' DM channels are excluded (archive discovery is `search_agent`).

`kill(child)`: parent-side force-archive — engine-synthesized failure report
to the killer, degraded handoff, archive transaction. This is the parent's
answer to a child that blocks its report gate.

`task` schema: `resident` and `background` fields are removed; `members[]`
fan-out remains; the result carries, per member, handle + session + DM
channel id. `task_id` resume is removed (revival supersedes it).

## 7. Archive index and search_agent

A store-derived table (not folded into the live projection), keyed by team
root, one row per archived agent:

| Column | Source |
| --- | --- |
| handle, session, stable_agent_id, agent_type | registration lineage |
| description, directive_digest | spawn intent |
| goal, current_state, pending | parsed sections of the latest `HandoffCommitted` |
| degraded | handoff flag |
| archived_at, reason | `AgentArchived` |

`search_agent(query, [agent_type], [has_pending], [limit])` — scope is
**caller's direct archived children only**; structured filtering over the
section digests. The tool returns rows sufficient to choose a revive target
(handle, goal, pending, degraded, archived_at); revival itself is a `dm`.

The index is rebuildable from the event log (derived, not authoritative). It
is not part of `Projection`, keeping the live projection bounded.

## 8. Visibility matrix

| Viewer | Sees |
| --- | --- |
| Agent | its DM peers: parent (always live, §1 invariant) and live direct children; group channels as pipes without member lists. No siblings, no grandchildren, no archive. |
| Agent (via `search_agent`) | its own archived direct children as handoff digests. |
| User (TUI) | the complete agent tree at every depth, live status, and archived entries as history. |
| Event log / `Projection` | everything, globally — the single replay truth. |

The agent-side narrowing is a **read filter** over tools; the projection and
SDK `TeamProjection` mirror keep folding the full tree (conformance test
updated, not relaxed).

## 9. Event model changes

| Change | Event | Notes |
| --- | --- | --- |
| new | `ChannelCreated { session, channel, kind, members }` | `kind: Group\|Dm`; minted ids |
| new | `SubagentReported { session, root, child, handle, outcome, report }` | terminal report; workflow consumes this (§10) |
| new | `HandoffCommitted { session, handle, generation, doc, degraded }` | on the child session log |
| new | `AgentArchived { session, root, handle, child, reason }` | sole archive marker; projection removes roster row |
| new | `AgentRestarted { session, root, handle, child, epoch }` | revive marker; distinguishes from fresh spawn |
| changed | `AgentRegistered` | `mode` minted as Resident only; `SubagentMode::Transient` deleted |
| changed | `MailSent` | `MailEndpoint::Channel` only (`Handle` variant deleted) |
| changed | `ChannelJoined` | emitted for group-channel membership only |
| removed | — | `TeamEvidenceEnvelope` projection, evidence system messages, transient `MemberFinished`-on-join semantics |

`MemberFinished` is retained only as the engine-synthesized terminal marker
inside archive/kill transactions (keeps the existing tree projection
semantics); `MemberStatusChanged` is retained for running transitions.

## 10. Workflow integration

Per [ADR-0017](../adr/0017-workflow-on-unified-substrate.md): control plane
unchanged; execution rides §2. Stage completion is `SubagentReported`
consumption (bus subscription in `WorkflowControl`, store-tail fallback for
recovery). Outcome+reason maps to the existing failure classes; a retry edge
is a revive (§4) whose DM body is the retry prompt and whose arming context is
the stage's handoff. `run_pre_admitted_team_with_workflow` and the transient
member admission variants are deleted. Verifier gating and engine-owned stop
decisions are unchanged.

## 11. Deletions (exhaustive)

- `run_team`, `run_pre_admitted_team*` variants, blocking joins
  (`crates/hya-core/src/subagent.rs` resident-execution half survives as the
  only member-run path).
- `TeamEvidenceEnvelope`, `project_envelope*`, evidence system messages.
- `uses_durable_admission_owner` routing and
  `ForegroundTransientAdmissionOwner` foreground/background reply modes (the
  preparation/intent rigor is merged into §2).
- `roster`, `channels`, `join`, `leave` tools; named user channels.
- `MailEndpoint::Handle`; path-arithmetic `in_scope` (replaced by membership
  + direction gates).
- `SubagentMode::Transient`; `task_id` resume; `resident`/`background` task
  fields.
- `subagent.max_depth` config key.
- Transient workflow execution path (§10).
- ADR-0002's idle-forever stance and ADR-0011's sibling scope (superseded).

## 12. Invariants checklist

1. Archive is the only exit; it is atomic with claim release, budget refund,
   roster removal, and group-membership removal.
2. The report gate (inbox drained ∧ children archived) is checked at call time
   and re-checked in the archive transaction.
3. A live agent's parent is live (upward DM never hits an archive).
4. Revival is only via downward DM from the direct parent; it bumps the claim
   epoch, re-debits budget, and never replays transcripts into model context.
5. Every terminal path produces a report (model-issued or engine-synthesized)
   and a handoff (model-written or degraded).
6. Journal claim precedes governor debit; refund is exactly-once via
   `logical_released`; archive is the only refund trigger for a completed
   lease.
7. Orchestration tools are unadvertised at depth 2 and rejected at admission;
   the constant is not configurable.
8. Channel authorization = membership + direction, evaluated inside the store
   transaction; ids are facts, not capabilities.
9. The live projection excludes archived rows; the archive index is derived
   and rebuildable; the event log retains everything.

## 13. Implementation phases

Each phase lands TDD-first (one atomic failing test per behavior) with the
Rust gate `cargo fmt --all --check && cargo clippy --workspace --all-targets
-- -D warnings && cargo test --workspace --jobs 1 --exclude hya-e2e`, plus the
process gate for agent-surface phases
([docs/testing/agent-matrix.md](../testing/agent-matrix.md)).

- **Phase 1 — lifecycle core.** Events (§9), report gate, handoff pipeline,
  archive transaction, revive, unified admission (§2), budget lease semantics,
  `kill`, root teardown force-archive. Surfaces: `hya-core` (subagent/
  resident/engine), `hya-app` (supervisor unification), `hya-tool` (task
  reshape, report tool).
- **Phase 2 — channel plane.** Channel minting/kinds, write gates, `dm`/
  `broadcast`/`list_channel`, `search_agent` + archive index, tool deletions,
  depth advertisement filter, TUI/SDK presentation (full tree + archive
  history).
- **Phase 3 — workflow migration.** `SubagentReported` consumption, retry =
  revive, deletion of the transient workflow path; matrix update.

## 14. Test surface impact

- `crates/hya-core/tests/turn_loop.rs`, resident/subagent unit seams: re-based
  on episode semantics (arm/report/archive/revive cycles, gate rejections,
  degraded handoffs).
- `crates/hya-app/tests/spawn_admission.rs`, `nested_spawn_tree.rs`: unified
  path only; journal-finalizes-at-registration.
- `crates/hya-proto`: projection fold tests for the new events; legacy
  flat-mailbox fixture retired (breaking mandate).
- `crates/hya-sdk/tests/team_mirror_conformance.rs`: mirror updated for
  channel kinds + archive rows; conformance maintained, not relaxed.
- `crates/hya-e2e` p08/p09: rewritten for non-blocking task + report-driven
  completion; new cases: revive-via-DM, depth-2 tool absence, kill, budget
  refund ledger.
- `packages/hya-tui-ts`: team/channel presentation tests.
