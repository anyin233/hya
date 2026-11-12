# Admission journal and spawn governor

This document is the paired contract for **durable spawn admission**
(SQLite `admission_journal` in `hya-store`) and the **in-memory
`SubagentGovernor`** budget used by `SessionEngine` when spawning subagents.

The journal persists operation deduplication, lifecycle, actor fencing, and
refund evidence. The process-local governor enforces the spawn budget.

Related storage overview: [storage.md](storage.md).

## Why this exists

Nested fan-out must not:

- double-debit the per-root spawn budget under concurrent finalizers
- lose track of in-flight admissions across process restart / actor takeover

The journal is the durable truth for operation lifecycle. The governor is a
process-local reservation of units against that truth.

## Schema: `admission_journal`

The table is created by the session store's single `0001_init.sql` baseline.
Each operation owns exactly one row. The journal is not a runnable queue.

| Column | Constraint / notes |
| --- | --- |
| `operation_id` | BLOB PK, length 16 |
| `source_tool_call_id` | BLOB NOT NULL UNIQUE, length 16 |
| `root_session_id` | BLOB NOT NULL (team-root storage key for the spawn budget) |
| `request_fingerprint` | BLOB NOT NULL, length **32** |
| `state` | TEXT NOT NULL CHECK IN (`accepted`, `started`, `completed`, `cancelled`, `aborted`) |
| `admission_units` | INTEGER NOT NULL `> 0` |
| `logical_released` | INTEGER NOT NULL DEFAULT 0, CHECK 0/1; marks a started debit's release |
| `terminal_reason` | TEXT, nullable |
| `created_at` / `updated_at` | INTEGER NOT NULL, unix millis |
| `actor_id` | BLOB, nullable |
| `actor_epoch` | INTEGER, nullable, `> 0` if present |

Indexes: `admission_journal_root_state(root_session_id, state)` and
`admission_journal_actor_state(actor_id, actor_epoch, state)`.

## Store types

### `AdmissionState`

| Variant | Wire string | Terminal? |
| --- | --- | --- |
| `Accepted` | `accepted` | no |
| `Started` | `started` | no |
| `Completed` | `completed` | **yes** |
| `Cancelled` | `cancelled` | **yes** |
| `Aborted` | `aborted` | **yes** |

`is_terminal()` is true for `Completed` \| `Cancelled` \| `Aborted`.

### Input / record types

| Type | Fields |
| --- | --- |
| `AdmissionClaim` | `operation_id`, `source_tool_call_id`, `root_session`, `request_fingerprint: [u8; 32]`, `admission_units`, `actor_claim: Option<ActorClaim>` |
| `AdmissionActorBinding` | `actor_id: SessionId`, `actor_epoch: ActorEpoch` |
| `AdmissionRecord` | `operation_id`, `source_tool_call_id`, `root_session`, `request_fingerprint`, `state`, `admission_units`, `actor: Option<AdmissionActorBinding>`, `logical_released`, `terminal_reason`, `created_at`, `updated_at` |

### Outcome enums

| Enum | Variants |
| --- | --- |
| `AdmissionClaimOutcome` | `Claimed(AdmissionRecord)`, `Existing(AdmissionRecord)` |
| `AdmissionStartOutcome` | `Started(AdmissionRecord)`, `Existing(AdmissionRecord)` |
| `AdmissionTerminal` | `Completed`, `Cancelled`, `Aborted` → map to matching `AdmissionState` |
| `AdmissionFinalizeOutcome` | `record`, `release_required: bool` — **true only for the process that terminalized a started/debited operation** (see `logical_released`) |

## Store API

| Method | Role |
| --- | --- |
| `claim_admission` | Insert operation as `accepted`; idempotent conflict → `Existing` or `OperationIdConflict` if fingerprints disagree |
| `admission` | Load one operation's record |
| `start_admission` | `accepted` → `started` for a claim |
| `finalize_admission` | Terminalize operation; sets `logical_released` when previous state was `started` |
| `recover_nonterminal_admissions` | Abort non-actor accepted/started rows at startup; only started rows mark a released debit |
| `abort_recovered_actor_admissions` | Abort nonterminal rows for a recovered actor claim; rows that were started get `logical_released` for governor refund |
| `terminal_released_operations_for_root` | List terminal operation IDs with `logical_released = 1` for a root budget |
| `nonterminal_admissions_for_root` | List nonterminal rows for a root session (root-turn cleanup) |

For example, claim an `AdmissionClaim` with `admission_units: 2` and a stable
operation/tool-call identity, reserve two governor units, then call
`start_admission(operation_id, actor_claim)`. On completion call
`finalize_admission(operation_id, AdmissionTerminal::Completed, "completed", actor_claim)`;
refund only when the returned `release_required` is true. Repeating the same
claim returns `Existing`; changing its immutable request returns
`StoreError::OperationIdConflict`.

`claim_admission` inserts state `'accepted'` immediately — durable claim happens
before any governor debit.

## Engine: spawn admission lifecycle

Source: [`crates/hya-core/src/engine/admission.rs`](../../crates/hya-core/src/engine/admission.rs).

### `SpawnAdmissionOutcome`

| Variant | Meaning |
| --- | --- |
| `Started` | Journal moved to `started`; governor units reserved (if a governor is installed) |
| `Existing(AdmissionState)` | Operation already claimed / reserved |
| `Overloaded` | Governor `try_reserve_operation` returned overloaded |
| `MaxDepth` | `depth + 1 > max_depth` |
| `Cancelled` | Cancel token fired after durable claim, before debit |

### `begin_spawn_admission` order (fixed)

1. **Durable claim first** — `store.claim_admission` → on `Existing`, return
   `Existing(state)` without touching the governor.
2. **Cancel check** — if cancelled, `finalize_spawn_admission(..., Cancelled, "cancelled before debit")` → `Cancelled`.
3. **Depth check** (only when a governor is present) — if
   `depth.saturating_add(1) > max_depth`, finalize `Aborted` with reason
   `"maximum subagent depth exceeded"` → `MaxDepth`.
4. **Governor reservation** — `try_reserve_operation`:
   - `Overloaded` → finalize `Aborted` `"spawn admission overloaded"` → `Overloaded`
   - `Existing` / `Conflict` → `Existing(Accepted)` (no second debit)
   - `Acquired` → continue
5. **`start_admission`** — on success `Started`; on `Existing` release governor
   operation and return `Existing`; on store error release governor, attempt
   finalize `Aborted` with `"failed to persist started state"`, propagate error.

Without a governor, steps 3–4 are skipped; start still runs after cancel check.

### `finalize_spawn_admission`

1. `store.finalize_admission(operation_id, terminal, reason, actor_claim)`
2. If `outcome.release_required` **and** a governor is present →
   `governor.release_operation(operation_id)`

`release_required` tracks `logical_released` on the journal row: only the
finalizer that first terminalizes a **started** row may refund the governor.
That is what makes refund **exactly once** under concurrent finalizers.

### `finalize_root_spawn_admissions(root)`

Root-turn cleanup (depth-0 turn completion):

1. `governor.cancel_operations(root)` if present
2. For each `nonterminal_admissions_for_root(root)`,
   `finalize_spawn_admission(..., Cancelled, "root turn cleanup", None)`
3. `governor.release(root)` if present

Without this, a long-lived root session leaks per-run budget entries.

### `abort_recovered_actor_operations(recovered)`

Post-takeover path:

1. `store.abort_recovered_actor_admissions(recovered, "resident actor takeover")`
2. For each returned record with `logical_released == true`,
   `governor.release_operation(operation_id)`

Only rows that were started (and thus marked logically released on abort)
refund the process-local governor.

## Governor (process-local)

`SubagentGovernor` / `SubagentLimits` live in
[`orchestrator.rs`](../../crates/hya-core/src/orchestrator.rs). They are
**not** durable. Defaults and stream-permit classes (general vs reserved) are
documented in [runtime.md](runtime.md). This page only requires:

- reservation is attempted **after** a durable journal claim exists
- refund is gated by journal `release_required` / `logical_released`
- root cleanup cancels operations and releases the root budget entry

A logical release recorded during restart must not credit a new process's empty
governor (session-store resident recovery already encodes that rule).

## Invariants (checklist)

1. Durable claim before governor debit.
2. Distinct terminal reason strings for each early-exit path listed above.
3. Exactly-once governor refund via `logical_released` / `release_required`.
4. One row per operation; tool-call identity is unique.
5. Startup aborts non-actor accepted/started operations; actor recovery remains fenced.
