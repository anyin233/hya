# Database Guidelines

> Database patterns and conventions for this project.

---

## Overview

- `hya-store` uses `sqlx` with SQLite. `SessionStore` owns the connection pool
  and runs embedded migrations during `connect`/`connect_memory`.
- The append-only `event_log` plus `hya_proto::Projection` is canonical for
  session behavior. Auxiliary tables must not become a second session
  projection or emit parallel public events.
- The `admission_journal` is a narrow idempotency/admission control plane, not a
  runnable queue, effect log, or child-session source of truth.

---

## Query Patterns

- Prefer conditional single-statement transitions (`UPDATE ... WHERE state =
  ... RETURNING`) for compare-and-set state changes.
- Decode rows through one shared helper and reject corrupt enum/ID/fingerprint
  data with `StoreError::AdmissionData`.
- Durable admission claims compare every immutable field after
  `INSERT OR IGNORE`; an existing operation with any mismatch returns
  `OPERATION_ID_CONFLICT`.
- Startup recovery changes all nonterminal admission rows with one atomic
  `UPDATE ... RETURNING` statement. It must not read/dispatch each row first.

---

## Migrations

- Add monotonically numbered SQL files under `crates/hya-store/migrations/`.
- Migrations are additive for active control-plane state. Do not repurpose the
  dormant `session`, `team_run`, or `team_member` tables for admission.
- Keep CHECK/UNIQUE constraints aligned with Rust invariants and exercise every
  new migration through `SessionStore::connect_memory` tests.

---

## Naming Conventions

- Use snake_case table, column, and index names.
- UUID-backed operation/tool-call keys are 16-byte BLOBs; `SessionId` uses
  `SessionId::storage_key()` because new session IDs are not UUID-only.
- Request fingerprints are fixed 32-byte BLOBs. Admission states are explicit
  lowercase strings: `accepted`, `started`, `completed`, `cancelled`,
  `aborted`.

---

## Common Mistakes

- Never credit a fresh in-memory governor from a recovered row; old process
  permits disappeared on restart.
- Never write a public `Event` for admission-only state.
- Never let a terminal transition mutate to a different terminal.
- Never add generic lease/owner/resource columns without a current
  source-proven invariant and test.
