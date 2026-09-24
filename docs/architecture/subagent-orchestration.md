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
| Parent/ancestor `archive(target)` | tool call | turn cancelled (`cause: archived`) + degraded handoff + member row cancelled + archive (`archived_by_parent`); no report mail |
| Graceful drain (end of a one-shot run, SIGINT/SIGTERM, `serve` stop) | `ResidentSupervisor::drain` | turns drained, then degraded handoff + member row cancelled + archive (`shutdown`) |
| Root teardown | engine (root session delete) | force-archive every live descendant |
| Handoff model call fails | engine | deterministic degraded handoff (projection-derived), marked `degraded`; archive proceeds |

An idle agent without a report is **never** auto-archived: it may be awaiting
a parent answer. A stuck child surfaces through the parent's report gate,
which rejects reporting while any direct child is live; the parent then
`archive`s it.

Every archived agent stays readable (session log, channel history,
handoff) and is **wakeable**: mail from its parent to its handle or to their
DM channel revives it (§4) under the same handle and session. A drain archives
members instead of failing them, so a later run on the same database can wake
them.

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

**All subagents are resident (0.41.0).** Agent definitions carry no spawn
lifecycle: `spawn_lifecycle` is a removed manifest key, rejected by name with
`RemovedManifestKey` in `AgentBundle`, `AgentSetBundle`, and `WorkflowBundle`
agents (prepared catalogs written by earlier releases still decode; the value is
ignored). Every `task` spawn takes this path and registers `mode: resident`,
whatever the target definition is. The only one-shot member runner left is
the Workflow Stage without an `actor` key (§10), selected by the Workflow
shape, not by the Agent.

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

Mail the caller's current resident wake already injected into its turn
(`ResidentWorkStarted.inbox_through`) counts as read for the in-turn check.
The unread-mail rejection is actionable: it names each channel holding unread
mail (`#DM-… (N)`, or `N harness notice(s)`), the exact `read channel://<id>`
call, and the `send` / `wait` paths. Every agent can satisfy the gate because
the harness allocates the mail tools to every agent regardless of its bundle
`resource_view` (see the [coordination tools](agent-tool-surface.md#coordination-tools-allocated-at-startup)).

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

### 3.3 The lead is never archived

The lead — the user-started root agent at depth 0, handle `main` — is not a
member and never goes through §3: it has no report, no handoff, and no
archive. A failed lead turn (provider/runtime error) ends with
`MessageFinished { finish: error, cause: provider_error }` and the lead's
session stays live and resumable; members keep their DM back to it.
`archive_reported_agent` refuses the root outright (`the team lead is never
archived`), and the resident loop parks a failed main slot (roster `idle`,
`current_task: "turn failed: …"`) instead of archiving it. A graceful drain
parks the lead `idle` too.

### 3.4 Leader failure

When the lead's turn fails — never on a user kill/cancel, a SIGINT, or a
drain — the harness broadcasts a wrap-up notice to the whole team:

- **Recipients:** every live resident member on the team roster, at every
  depth (roster row not `done`/`failed`, active claim), excluding the lead.
- **Channel / author:** a direct `MailSent` per member on the team-root log,
  `from: "harness"` (`hya_proto::HARNESS_HANDLE`; not an agent, never on the
  roster, bypasses the hierarchy reach rule and the channel delivery policy),
  written by `SessionStore::append_harness_mail`.
- **Body:** `LEADER FAILED: your team lead's turn ended with an error and the
  lead will not respond (<error, ≤400 chars>). Wrap up now: finish or commit
  the unit you are on, send your report, then stop. Do not start new work.`
- **Delivery:** a member mid-turn sees it through in-turn steering
  (`[mail from harness] …` on its next tool result); an idle member is woken
  for one turn.
- **Synthesis:** the dead lead gets no automatic turn — a queued `TEAM
  QUIESCED` synthesis is dropped and mail wakes of `main` wait — until the user
  resumes the lead (see [Runtime — Leader failure](runtime.md#leader-failure)).

### 3.5 Storage

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
| Group | `#announce-{8}` | leader + direct reports | leader only (`send #…`) | per unit; membership ends at archive |
| DM | `#DM-{8}` | exactly two (vertical pair) | both (`send`) | created at spawn (top-down); **persists across archive/revive** |

- 8 random `[a-zA-Z0-9]` chars, collision-checked within the team root's
  channel table, re-minted on collision; minted ids are event facts
  (`ChannelCreated`), so replay is stable without derivation.
- Randomness is naming, not authorization: the write gate checks membership
  and role inside the store transaction.
- Group channels expose **no member list** through any tool. DM channels
  always expose the peer identity (derived from membership, not from input).

### 5.2 Write gate (send / report delivery)

```
send(channel?, body):
  acting identity from session context (never model input)
  handle/DM channel ⇒ private mail (child default: parent DM; archived child ⇒ revive §4)
  group channel ⇒ MailSent{Channel(#announce-…)} to live members; leader-only
  archived members are not members; nothing is queued for them
  channel omitted ⇒ role default: led unit group channel, else parent DM
  neither exists ⇒ typed error (advertised at L2, errors at runtime)

report(result):
  gate §3.1 ⇒ handoff §3.2 ⇒ archive txn §3.3 ⇒ report mail on the DM channel
```

`MailEndpoint::Handle` is deleted; all delivery is `MailEndpoint::Channel`.
Old logs containing handle-addressed mail or named channels are not guaranteed
to fold — accepted breakage.

### 5.3 Wake delivery

Unchanged seam: bus `MailSent` → supervisor `on_mail` → channel members minus
sender → `pending` + notify; busy recipients coalesce into one follow-up turn.
A recipient whose session already has an active turn — including a lead whose
turn was started by `exec`/`serve` rather than by the supervisor — is busy: the
wake waits for that turn to end (single active turn per session, see
[Runtime](runtime.md#single-active-turn-per-session)). Quiescence (`TEAM
QUIESCED`) likewise fires only once no member, the lead included, has a turn
in flight, and is delivered to the lead at its next turn boundary.
New: membership excludes archived handles at fold time, and a DM whose
recipient is archived routes to the revive path before wake.

## 6. Tool surface and depth policy

| Plane | Tools | Depth 0/1 (main, L1) | Depth 2 (L2) |
| --- | --- | --- | --- |
| Orchestration | `task`, `list_agents`, `workflow`, `search_agent`, `archive` | advertised | **not advertised** |
| Communication | `send`, `list_channel`, `report` (`report`: subagents only) | advertised | advertised (group-default send errors: leads nobody) |
| Waiting | `wait` | advertised | advertised (waits on mail when it has no subagents) |
| Coding/etc. | read/write/edit/bash/… | advertised | advertised |

These planes are **allocated by the harness** when an agent starts, not
declared by its bundle: `report`, `wait`, `send`, `list_channel`, and channel
reads go to every agent (channel tools when the channel family is loaded), and
`task`/`archive` to every agent with spawn rights. A bundle `resource_view`
narrows only the coding/etc. plane; `deny` cannot remove `report`. See
[agent-tool-surface.md](agent-tool-surface.md#coordination-tools-allocated-at-startup).

Enforcement is two-layer, engine-owned:

1. **Advertisement filter** keyed on session lineage depth (engine, not
   catalog/resource policy — a bundle must not be able to reintroduce
   orchestration tools at L2).
2. **Admission depth check** (§2 step 2) as the API-level backstop.

Deleted tools: `roster`, `channels`, `join`, `leave` (folded into
`list_channel`). New tools: `dm`, `broadcast`, `list_channel`, `report`,
`search_agent`, `archive` (was `kill` before 0.41.0).

`list_channel` output: channel id, kind, can-speak, unread count, and for DM
channels the peer identity; only channels the caller belongs to; archived
peers' DM channels are excluded (archive discovery is `search_agent`).

`archive(target, reason?)` (0.41.0; replaces `kill`): stop and archive a live
descendant. The target is a handle, a caller-relative leaf, or a session id;
its live descendants archive first (deepest first). An in-flight turn is
cancelled with `cause: archived` via `SessionEngine::stop_turn` (waits up to
`DRAIN_DEADLINE`, then closes a straggler's message), the slot owes no more
turns, and `archive_stopped_agent` commits degraded handoff → member row
`cancelled` → claim release → `AgentArchived { archived_by_parent }`. No
report mail is sent (the archiver knows). The lead is never archivable; an
unknown target lists the caller's live subagents. This is the parent's answer
to a child that blocks its report gate or is no longer needed.

`wait(targets?, mode?, timeout_secs?)` (0.41.0) blocks the caller's turn —
typically the lead's own — until its subagents finish, `any` or `all`, bounded
by a timeout (default 600 s, max 1800 s). A subagent **finishes** only by
reporting or being archived; an idle member is never finished. Each call is
measured against a baseline taken at its start: a target that had already
reported or been archived is returned under `already_finished` and never wakes
the wait (every target already finished → `nothing_to_wait_for` at once); a
member woken again by mail after its report is working until its next report
(its terminal handoff generation, bumped by every archive, tells the two
finishes apart); a member whose turn ended without a report while nothing is
queued for it — it will not continue until mailed — wakes the wait once as
`stalled` and a repeated wait blocks. Only `timeout_secs: 0` returns the
current state without blocking. The waiter subscribes to the engine bus and
re-evaluates on team-lifecycle events (`AgentActivityChanged`,
`SubagentReported`, `AgentArchived`, `MailSent`, …) against the supervisor's
in-memory slot state, so it is woken **inside** the running turn; it never
relies on a resident wake of the lead, which would queue behind that same turn
(single active turn per session). A report accepted mid-turn keeps the member
"finishing" until the archive commits. Cancelling the turn aborts the wait. The
channel-tools family overrides `wait` (explicit `overrides` in its exposure
policy) with a version that also returns on new mail for the caller — harness
mail such as `LEADER FAILED` included — reporting `woke_by: mail`. Mail is new
only past the caller's durable `MailConsumed` inbox cursor, and the wait
commits that cursor for the mail it returns (and for finished targets' report
mail), so neither a later `wait`, nor the `[NEW MAIL]` steer notice, nor a
resident wake delivers it again. The steer notice rebuilds its pending mail
from that durable cursor with the channel view refreshed, so mail on a DM
channel minted after a long lead turn began is steered too. Full contract:
[Agent tool surface](agent-tool-surface.md).

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
| User (client UI) | the complete agent tree at every depth, live status, and archived entries as history. |
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
| new | `AgentArchived { session, root, handle, child, reason }` | sole archive marker; projection removes roster row. `reason`: `reported`, `root_teardown`, `archived_by_parent` (`archive` tool), `shutdown` (graceful drain), legacy `killed` |
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
   and a handoff (model-written or degraded). The lead has no terminal path:
   it is never reported, handed off, or archived (§3.3).
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
  depth advertisement filter, client/SDK presentation (full tree + archive
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
- `crates/hya-e2e` p08/p09: rewritten for non-blocking task + report-driven
  completion; new cases: revive-via-DM, depth-2 tool absence, kill, budget
  refund ledger.
- Client-side team/channel presentation tests return with the future
  `hya-sdk-v1` frontend (the legacy `hya-sdk` mirror conformance suite and the
  removed TUI's presentation tests retired with those components).
