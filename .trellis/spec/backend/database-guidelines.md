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
- Exception: `BundleRegistry` owns the separate
  `<data_root>/bundles/registry.sqlite3` installed-package control-plane DB;
  builtins are not rows, and it is not a session projection.

---

## Query Patterns

- Prefer conditional single-statement transitions (`UPDATE ... WHERE state =
  ... RETURNING`) for compare-and-set state changes.
- Decode rows through one shared helper and reject corrupt enum/ID/fingerprint
  data with `StoreError::AdmissionData`.
- Durable admission claims compare every immutable field after
  `INSERT OR IGNORE`; an existing operation with any mismatch returns
  `OPERATION_ID_CONFLICT`.
- Startup recovery changes all non-actor nonterminal admission rows with one
  atomic `UPDATE ... RETURNING` statement. Actor-bound rows remain for the
  recovered claim's fenced transaction; neither path reads or dispatches a row
  first.
- Resident actors use one indexed `resident_actor_claim` row keyed by their
  persisted agent-session `SessionId`. Claim/recover/release are transactional
  compare-and-set operations over the full actor/epoch/owner tuple.
- Resident canonical mutations validate the current claim in the same SQLite
  transaction as event append or admission transition. Event-bus publication
  occurs only after commit.
- Full-tuple claim release terminalizes that exact actor/epoch's nonterminal
  admissions in the same writer transaction before making the claim reusable;
  only returned first-release rows may refund an in-memory governor.

---

## Migrations

- Add monotonically numbered SQL files under `crates/hya-store/migrations/`.
- `BundleRegistry` embeds its separate migrations from
  `crates/hya-store/bundle_migrations`; migration tests use a `BundleRegistry`
  temp DB.
- Migrations are additive for active control-plane state. Do not repurpose the
  dormant `session`, `team_run`, or `team_member` tables for admission.
- Keep CHECK/UNIQUE constraints aligned with Rust invariants; exercise
  `SessionStore` migrations through `SessionStore::connect_memory` tests and
  `BundleRegistry` migrations through its temp-DB tests.
- Actor epoch is monotonic integer state. The claim table must not acquire TTL,
  heartbeat, wall-clock, or background-expiry columns.

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
- Never let claim-less root cleanup/finalization mutate an actor-bound
  admission. Startup has a separate fail-closed recovery transition; ordinary
  actor transitions require the matching claim.
- Never credit a new process governor for a recovered old-process debit, retry
  work that crossed `ResidentWorkStarted`, or conflate actor epoch with runtime
  configuration generation.
