# Resident actor model and autonomous main agent

Subagents have a **hybrid** lifecycle: `transient` (the default — spawn, run one turn, return a
bounded summary, parent blocks) stays unchanged, and a new opt-in `resident` mode makes a subagent
a long-lived, event-driven **actor** that idles at zero token cost and wakes on inbound mail to run
exactly one turn. The **main agent is also an actor**, woken by child mail, so a team runs
hands-free to completion. We chose event-driven wake over a polling goal-loop because 100+ agents
each looping would burn tokens continuously and need a per-agent stop condition; event-driven
actors cost nothing while idle and reach a natural **quiescence** (all idle + no mail in flight)
that wakes the main agent to synthesize.

## Consequences

- Because an idle actor swarm could deadlock (everyone waiting) or run away (agents messaging
  forever), the design carries two guards that are *not* optional: a race-free quiescence detector
  (the fire decision happens in the same locked section that observes no pending work, with a
  `work_seq` termination guard so a no-new-work synthesis doesn't re-fire) and per-team
  turn/message budgets that cancel a runaway team.
- Resident spawns are **non-blocking** (parent gets the handle and continues), which diverges from
  the transient `run_team` join model — the two spawn paths coexist.
- User input never wakes a resident: residents wake *only* via mail. This is what lets the TUI bind
  all user input to the main agent (see ADR-0003).

## Crash recovery and fencing (0.34.7)

Each resident uses its already-persisted agent session ID as its stable actor
identity. SQLite stores one active/released claim for that identity with a
monotonic `ActorEpoch` and a random per-process `OwnerRunId`. This is a
transactional incarnation fence, not a time lease: there is no TTL, heartbeat,
wall-clock expiry, background lease supervisor, distributed lock, consensus,
or active-active/HA claim.

Startup stays closed to spawn/send/wait while it:

1. increments every active resident epoch, invalidating old capabilities;
2. aborts old-epoch running work and resident-bound nonterminal admissions;
3. replays the canonical roster, inbox, and session projections;
4. recreates the existing resident tasks and schedules only committed work that
   had not crossed `ResidentWorkStarted`.

`ResidentWorkStarted` is the single durable boundary between queued work and a
turn that may dispatch a provider, tool, or child. A completed turn advances
the projected inbox cursor. A crash after that marker aborts the running work
without retry; mail committed after the marker remains queued for the new
epoch. Repeating startup recovery may advance the epoch again, but does not
duplicate terminal events or admission refunds.

### Explicit stop and inbox cursor

An `AgentActivityChanged` with `status = Failed` **and**
`current_task == "resident stopped"` (exact literal) is treated by the shared
reducer as an **explicit stop**, not a generic failure. For a resident roster
entry it jumps `resident_cursor` to the **full inbox length** so a later
restart of that handle does not replay mail the stopped actor never needed to
see.

That literal is load-bearing: `SessionStore::finalize_resident_stop` writes it
via `finalize_resident_failure(..., "resident stopped")`, and the reducer keys
on the same string. Other failure reasons (for example recovery's
`"aborted by resident recovery"`) do **not** trigger this full-inbox cursor jump.

### Terminal cleanup events for a lost / stopped actor

When recovery aborts in-flight work for a resident that had crossed
`ResidentWorkStarted` (or when stop/failure terminalizes the actor session),
`resident_effect_terminal_events` appends exactly:

| Event | When |
| --- | --- |
| `MemberFinished { status: Cancelled, summary: <reason> }` | Every member still `Spawning` or `Running` on the **actor** session |
| `ToolError` with `value: { "code": "STALE_ACTOR_CLAIM" }` and `message_text: <reason>` | Every tool part still `Pending` or `Running` on an unfinished assistant message |
| `MessageFinished { finish: Cancelled }` | Every unfinished assistant message |

Clients can key off the `STALE_ACTOR_CLAIM` code to distinguish takeover/stop
cleanup from a genuine tool failure. The same helper is used for both recovery
(`reason = "aborted by resident recovery"`) and explicit stop/failure paths.

### `finalize_resident_stop` / `finalize_resident_failure`

Both live on `SessionStore` (`hya-store` mailbox module):

1. Fence the actor claim (`BEGIN IMMEDIATE`).
2. **Idempotent early return:** if the claim is already stale **and** a matching
   `released` claim already exists with the roster entry in `Failed`, return
   empty envelopes and admissions (no duplicate terminalization).
3. Append the terminal cleanup events above for the actor session.
4. Abort `accepted`/`started` admissions for that actor/epoch (`state = aborted`);
   rows that were `started` set `logical_released = 1` for exactly-once governor
   refund.
5. Append root `AgentActivityChanged { status: Failed, current_task: Some(reason) }`
   (`reason` is `"resident stopped"` for explicit stop).
6. Flip `resident_actor_claim` to `released` for the full claim tuple.

### `RecoveredResidentWork` / `RecoveredResidentOutcome`

`recover_resident_actor` returns `RecoveredResidentOutcome`:

| Field | Meaning |
| --- | --- |
| `work` | Classification of what the new process should do next |
| `envelopes` | Envelopes to publish **after** commit |
| `admissions` | Aborted admission rows (refund when `logical_released`) |

`RecoveredResidentWork`:

| Variant | Meaning |
| --- | --- |
| `Idle` | No pending inbox work and no interrupted turn |
| `Queued { inbox_cursor }` | Committed mail (or pending user turn while idle) remains after the cursor |
| `AbortedRunning { inbox_cursor, queued_after }` | Crossed `ResidentWorkStarted`; running work aborted; `queued_after` if inbox grew past the marker |

Resident event appends, mailbox mutations, child transitions, spawn admission,
and tool-result commits validate the full actor claim in the same SQLite
transaction as their canonical mutation. Event-bus publication happens only
after commit. An old process may finish local computation, but its late result
returns typed `StaleActorClaim` and cannot advance replay/projection state.
Releasing a claim atomically aborts nonterminal admissions bound to that exact
actor/epoch before the tuple becomes reusable, so a budget kill cannot strand
an operation outside startup recovery.
Transient work keeps its existing event shapes and performs no actor-claim
lookup.

This provides deterministic single-process crash recovery and canonical-state
fencing. It does **not** make filesystem, network, or third-party API effects
externally exactly once; effects that occurred before takeover cannot be
reversed. It also does not provide HA/active-active operation, time leases,
automatic retry of running/in-flight work, or proof of the planned 100-agent
capacity boundary.
