# Episode admission and capacity

Each active `resident_actor_claim` is one capacity lease. This makes the live
member limit durable across root turns and process crashes, and releases it in
the same transaction that ends the episode. Idle workers, parked Workflow actors
and resident verifiers all count. The main/root agent does not hold a lease.

## Usage

Configure `subagents.per_run_budget: 2` (or `HYA_SUBAGENT_BUDGET=2`) to allow two
live members per root. Two `task` spawns may register; a third returns a typed
`overloaded` tool error. Archive one member to admit another. Mailing an archived
member consumes a lease when it revives and fails with overload if none is free.
A completed root turn does not release members' leases.

A `task` batch registers every member or none. Resolution of agents, models,
resources and sidecar factories precedes the store call. A write failure in any
member rolls back the entire batch, including lazy root registration. Once the
transaction commits, the supervisor installs all episodes under its claim-write
serialization before awaiting initialization hooks. Each episode publishes its
registration envelopes after session-start hooks and before it can publish its
archive.

Workflow admission reads live capacity and checks its compiled worst-case
activation count against the remaining limit. This is a preflight check, not a
reservation: concurrent `task` calls can take capacity, and a later Workflow
registration or revival can fail with overload. There is no budget refund guard.

## Store interface

`resident_actor_claim` has these columns:

| Column | Type and contract |
| --- | --- |
| `actor_id` | BLOB primary key, member SessionId storage key |
| `root_session_id` | BLOB NOT NULL, owning team root |
| `epoch` | INTEGER NOT NULL, positive and monotonically increasing |
| `owner_run_id` | BLOB NOT NULL, 16-byte runtime owner ID |
| `state` | TEXT NOT NULL, `active` or `released` |

`resident_actor_claim_root_state(root_session_id, state)` supports capacity
counts. The existing `(state, actor_id)` index supports startup recovery.

- `register_episodes(root: SessionId, owner: OwnerRunId,
  parent_claim: Option<&ActorClaim>, capacity: u64,
  registrations: Vec<EpisodeRegistration>) -> Vec<(ActorClaim, Vec<Envelope>)>`:
  one `BEGIN IMMEDIATE` transaction fences the parent, checks
  `live + registrations.len() <= capacity`, registers main if absent, and writes
  each claim plus canonical session/member/roster/channel/prompt events.
  `EpisodeRegistration { actor: SessionId, events: Vec<Event> }` is prepared by
  the supervisor; results retain input order, with lazy-main registration in
  the first member's envelope group. Group-channel lookup-or-create runs
  under the same writer reservation.
- `revive_episode(root, owner, capacity: u64, parent_claim, actor, from, to, kind,
  body, context_marker, policy)` checks one additional lease inside its existing
  fenced revival transaction. Already-live targets append mail without another
  lease. The remaining parameters/results are documented in
  [resident revival](subagent-orchestration.md#4-revival).
- `live_member_count(root: SessionId) -> u64` is a read-only count of active
  claims, used for Workflow preflight.
- `try_claim_new(root: SessionId, actor: SessionId, owner: OwnerRunId)` records
  the root on a standalone claim; runtime creation goes through registration or
  revival so the capacity check shares the transaction.
- `release_claim(claim: &ActorClaim) -> ()` fences and releases exactly that
  claim. Normal endings use `finalize_resident_archive`, which releases the claim
  with the terminal events. Repeated exact releases are harmless.

All store methods return `Result<_, StoreError>`. A capacity refusal is
`CapacityExceeded { root: SessionId, live: u64, requested: u64, limit: u64 }`;
the `task` boundary maps it to `SpawnError::Overloaded` / `ToolError::Overloaded`.
Mailbox send replies carry `MailboxError::Overloaded(String)`, which the `send`
tool also maps to `ToolError::Overloaded` (`error.type: "overloaded"`).
No registration events or claims commit on refusal.

## Governor and identity

`SubagentGovernor` retains the two provider-stream semaphores and the hardcoded
maximum depth. Team turn/message runaway counters live on `Team`. There is no
in-memory spawn ledger, admission journal, operation finalizer, or root-turn
budget reset. Startup fences existing active claims without changing capacity.

`ToolOperation` still carries the source call and actor capability; Workflow uses
its derived operation ID to name durable runs. Task calls are dispatched once:
provider retries happen only before any tool call or text, and startup closes
interrupted calls rather than dispatching their stored IDs. There is no spawn
operation deduplication API or conflict/already-handled error surface.
