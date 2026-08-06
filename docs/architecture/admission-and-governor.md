# Admission journal and spawn governor

This document is the paired contract for **durable spawn admission**
(SQLite `admission_journal` in `hya-store`) and the **in-memory
`SubagentGovernor`** budget used by `SessionEngine` when spawning subagents.

Schema and API must agree: capacity caps, state names, and intent size limits
are enforced in both SQL CHECK constraints and Rust. Prefer omitting a claim
over guessing one.

Related storage overview: [storage.md](storage.md).

## Why this exists

Nested fan-out must not:

- double-debit the per-root spawn budget under concurrent finalizers
- lose track of in-flight admissions across process restart / actor takeover
- promote queued work unfairly under load

The journal is the durable truth for operation lifecycle. The governor is a
process-local reservation of units against that truth.

## Schema: `admission_journal`

### Lifecycle of the table

| Migration | Change |
| --- | --- |
| `0004_admission_journal.sql` | Create journal: PK `operation_id`, states `accepted`/`started`/`completed`/`cancelled`/`aborted` |
| `0005_resident_actor_claim.sql` | Nullable `actor_id`, `actor_epoch` (epoch `NULL` or `> 0`) + index |
| `0006_admission_queue_states.sql` | **Rebuild**: composite PK `(operation_id, member_ordinal)`, add `queued`/`waiting`, `member_ordinal` / `batch_size` |
| `0007_admission_bindings.sql` | Binding + spawn-intent columns (all-or-nothing CHECK) |
| `0008_admission_fairness.sql` | `admission_sequence` / `promotion_sequence` + FIFO indexes |

### Effective columns (after 0008)

| Column | Constraint / notes |
| --- | --- |
| `operation_id` | BLOB, length 16; part of composite PK |
| `member_ordinal` | INTEGER `>= 0`, part of PK; **CHECK** `member_ordinal < batch_size` |
| `source_tool_call_id` | BLOB length 16; **UNIQUE** with `member_ordinal` |
| `root_session_id` | BLOB NOT NULL (team-root storage key for the spawn budget) |
| `request_fingerprint` | BLOB length **32** |
| `state` | CHECK IN (`queued`, `accepted`, `started`, `waiting`, `completed`, `cancelled`, `aborted`) |
| `admission_units` | INTEGER `> 0` |
| `logical_released` | INTEGER 0/1 — set when a **started** row is first terminalized (exactly-once refund flag) |
| `terminal_reason` | TEXT, nullable |
| `created_at` / `updated_at` | INTEGER millis |
| `actor_id` | BLOB, nullable |
| `actor_epoch` | INTEGER, nullable, `> 0` if present |
| `batch_size` | INTEGER `> 0` |
| `runtime_fingerprint_version` | INTEGER 0..=u32::MAX, nullable |
| `runtime_fingerprint` | BLOB length 32, nullable |
| `admission_binding_fingerprint_version` | INTEGER 0..=u32::MAX, nullable |
| `admission_binding_fingerprint` | BLOB length 32, nullable |
| `spawn_intent` | BLOB length **1..=1_048_576**, nullable |
| `admission_sequence` | INTEGER `> 0` or NULL; backfilled from `rowid` in 0008 |
| `promotion_sequence` | INTEGER `> 0` or NULL |

**All-or-nothing binding CHECK (0007):** the five binding columns
(`runtime_fingerprint_version`, `runtime_fingerprint`,
`admission_binding_fingerprint_version`, `admission_binding_fingerprint`,
`spawn_intent`) are either **all NULL** or **all present**.

### Indexes (fairness / promotion)

| Index | Purpose |
| --- | --- |
| `admission_journal_admission_sequence` | Partial **UNIQUE** on `admission_sequence` WHERE NOT NULL |
| `admission_journal_promotion_sequence` | Partial **UNIQUE** on `promotion_sequence` WHERE NOT NULL |
| `admission_journal_queued_admission_sequence` | Partial on `(state, admission_sequence)` WHERE `state = 'queued'` — FIFO scan for promotion |
| `admission_journal_root_promotion_sequence` | Partial on `(root_session_id, promotion_sequence)` WHERE NOT NULL |
| `admission_journal_root_state` | `(root_session_id, state)` |
| `admission_journal_actor_state` | `(actor_id, actor_epoch, state)` |

## Store types

### `AdmissionState`

| Variant | Wire string | Terminal? |
| --- | --- | --- |
| `Queued` | `queued` | no |
| `Accepted` | `accepted` | no |
| `Started` | `started` | no |
| `Waiting` | `waiting` | no |
| `Completed` | `completed` | **yes** |
| `Cancelled` | `cancelled` | **yes** |
| `Aborted` | `aborted` | **yes** |

`is_terminal()` is true for `Completed` \| `Cancelled` \| `Aborted`.

### Capacity caps

Rust constants (enforced before insert/promote; mirrored by count queries):

| Cap | Value | Counts states |
| --- | --- | --- |
| `MAX_ACTIVE_ADMISSIONS` | **100** | `accepted` + `started` |
| `MAX_NON_ACTIVE_ADMISSIONS` | **156** | `queued` + `waiting` |

Exceeding either returns `StoreError::AdmissionCapacityExceeded { active, non_active, requested }`.

### Intent size

`MAX_ADMISSION_INTENT_BYTES = 1_048_576` (1 MiB). Empty or oversized
`spawn_intent` is rejected in Rust and by the SQL CHECK on the column.

### Input / record types

| Type | Fields |
| --- | --- |
| `AdmissionClaim` | `operation_id`, `source_tool_call_id`, `root_session`, `request_fingerprint: [u8; 32]`, `admission_units`, `actor_claim: Option<ActorClaim>` |
| `AdmissionIntent` | `runtime_fingerprint_version: u32`, `runtime_fingerprint: [u8; 32]`, `admission_binding_fingerprint_version: u32`, `admission_binding_fingerprint: [u8; 32]`, `spawn_intent: Vec<u8>` |
| `AdmissionLaunch` | `record: AdmissionRecord`, `intent: AdmissionIntent` |
| `AdmissionActorBinding` | `actor_id: SessionId`, `actor_epoch: ActorEpoch` |
| `AdmissionRecord` | `operation_id`, `source_tool_call_id`, `root_session`, `request_fingerprint`, `member_ordinal`, `batch_size`, `state`, `admission_units`, `actor: Option<AdmissionActorBinding>`, `logical_released`, `terminal_reason`, `created_at`, `updated_at` |
| `AdmissionCounts` | `active`, `non_active`, `total` |

### Outcome enums

| Enum | Variants |
| --- | --- |
| `AdmissionClaimOutcome` | `Claimed(AdmissionRecord)`, `Existing(AdmissionRecord)` |
| `AdmissionBatchClaimOutcome` | `Claimed(Vec<AdmissionLaunch>)`, `Existing` |
| `AdmissionStartOutcome` | `Started(AdmissionRecord)`, `Existing(AdmissionRecord)` |
| `AdmissionTerminal` | `Completed`, `Cancelled`, `Aborted` → map to matching `AdmissionState` |
| `AdmissionFinalizeOutcome` | `record`, `release_required: bool` — **true only for the process that terminalized a started/debited operation** (see `logical_released`) |
| `AdmissionReleaseOutcome` | `finalized: Vec<AdmissionFinalizeOutcome>`, `promoted: Vec<AdmissionLaunch>` |

## Store API (15 methods)

| Method | Role |
| --- | --- |
| `claim_admission` | Insert single-member row as `accepted` (`member_ordinal=0`, `batch_size=1`); idempotent conflict → `Existing` or `OperationIdConflict` if fingerprints disagree |
| `claim_admission_batch` | Claim a batch of members with binding intents (capacity-checked) |
| `suspend_parent_and_claim_admission_batch` | Parent member → `waiting`, then claim child batch in one transaction |
| `queue_waiting_admission_member` | Move/create a member into `queued`/`waiting` path for later promotion |
| `admission` | Load one operation's primary record (`member_ordinal=0`, `batch_size=1`) |
| `admissions` | Load all member rows for an operation id |
| `admission_counts` | Active / non-active / total counts |
| `promote_queued_admissions` | FIFO promote from `queued` into active slots (uses fairness indexes) |
| `start_admission` | `accepted` → `started` for single-member claim |
| `start_admission_member` | Start a specific member ordinal |
| `finalize_admission` | Terminalize single-member row; sets `logical_released` when previous state was `started` |
| `finalize_admission_members` | Terminalize multiple members; may return promotions |
| `recover_nonterminal_admissions` | Startup recovery of nonterminal rows |
| `abort_recovered_actor_admissions` | Abort nonterminal rows for a recovered actor claim; rows that were started get `logical_released` for governor refund |
| `nonterminal_admissions_for_root` | List nonterminal rows for a root session (root-turn cleanup) |

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
4. Active cap 100 (`accepted`+`started`); non-active cap 156 (`queued`+`waiting`).
5. Spawn intent size 1..=1 MiB, all-or-nothing with binding fingerprints.
6. Member PK `(operation_id, member_ordinal)` with `member_ordinal < batch_size`.
7. FIFO promotion driven by `admission_sequence` under partial indexes.
