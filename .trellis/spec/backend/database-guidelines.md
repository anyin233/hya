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

---

## Scenario: Owner-Fenced Per-Agent Model Preferences

### 1. Scope / Trigger

- Trigger: changing Agent model persistence, runtime binding, root/subagent/
  Workflow/fixed-Agent model resolution, or `/tui/agent-models`.
- The preference table is backend control state. It is not a Session projection
  and must not emit a public `Event`.

### 2. Signatures

- Migration: `agent_model_preference(agent_id TEXT PRIMARY KEY, provider_id
  TEXT, model_id TEXT)` with length checks `1..=1024`, `1..=1024`, and
  `1..=4096`.
- Store: `list_agent_model_preferences()`,
  `upsert_agent_model_preference(owner, row)`, and
  `remove_agent_model_preference(owner, agent)`.
- App: `PersistentAgentModelControl::set(binding, agent_id, identity)` and
  `effective_model(binding, agent_id, base_model)`.
- Server: `GET /tui/agent-models`; `PUT /tui/agent-models/:agent_id` with
  `{ "preference": { "providerID", "modelID" } }` or
  `{ "preference": null }`.

### 3. Contracts

- Store provider and provider-local model in separate columns. Never split a
  model-local slash during load, validation, API conversion, or execution.
- A mutation validates one exact Agent and exact provider-catalog row against
  one `TurnBinding`. Direct model or category policy rejects a set but still
  permits clear.
- Under one async mutation lock: commit with the matching runtime owner, update
  the complete in-memory map, then publish with infallible replacement. Never
  do a fallible database re-read after commit and before publication.
- `TurnBinding` captures one immutable `Arc` map. Existing admissions and
  residents retain it; only later bindings observe a successful mutation.
- Effective order is base < valid remembered < configured category/direct <
  inline/request/spawn/Workflow Stage category/direct. Direct/category
  presence suppresses memory even when the configured route does not resolve.
- Root Sessions, ordinary spawns, unassigned Workflow roles, Title, Summary,
  and both Compaction paths use the same captured preference rules. Request,
  CLI, Session hydration, variant, and Stage overrides are never persisted.
- Admission fingerprints include only preferences that can affect that exact
  request. Unrelated Agents and roles with explicit request/Stage routes are
  excluded.

### 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Missing/unknown Agent | `404 AGENT_MODEL_UNKNOWN_AGENT` |
| Set on direct/category-configured Agent | `409 AGENT_MODEL_CONFIGURED` |
| Empty, oversized, malformed, or non-catalog identity | `400 AGENT_MODEL_INVALID_REQUEST` or `AGENT_MODEL_UNAVAILABLE` |
| Missing `preference` key or unknown PUT field | `400 AGENT_MODEL_INVALID_REQUEST` |
| No installed control | `503 AGENT_MODEL_CONTROL_UNAVAILABLE`; bootstrap capability is `false` |
| Owner/store/runtime failure | `503 AGENT_MODEL_CONTROL_FAILURE`; published map stays unchanged |
| Stored identity becomes stale | Retain it for display, mark unavailable, and use the existing base path |
| Explicit JSON `null` | Idempotent clear; return the post-commit default/configured row |

### 5. Good / Base / Bad Cases

- Good: Agents A and B commit different rows in one file-backed Session DB;
  restart restores both, and old bindings still use their prior map.
- Base: no row leaves the prior default model path unchanged; a memory DB loses
  rows on restart; another DB path has an independent map.
- Bad: store `provider/model` in one column, publish before commit, rebind after
  commit to build the response, or hash every Agent preference into every
  admission.

### 6. Tests Required

- Store tests cover deterministic list order, upsert, clear, A/B isolation,
  owner fencing, concurrent disjoint mutations, bounds, and file reopen.
- Core/app tests cover immutable old/new bindings, exact stale fallback, all
  precedence layers, failed-write publication safety, relevant fingerprints,
  Workflow routes, and configured categories.
- Fixed-Agent tests capture provider requests for Title, Summary, native
  Compaction, and local Compaction.
- Server tests assert bootstrap/list/set/clear, model-local slashes, one-binding
  root creation, exact 400/404/409/503 bodies, and empty-control behavior.

### 7. Wrong vs Correct

#### Wrong

```rust
runtime.publish_agent_model_preferences(next);
store.upsert_agent_model_preference(owner, row).await?;
let rebound = runtime.bind_turn(workdir)?;
```

This can publish a failed write and return state from a different catalog or
preference generation.

#### Correct

```rust
let _guard = mutation.lock().await;
store.upsert_agent_model_preference(owner, row).await?;
map.insert(agent_id, model);
runtime.publish_agent_model_preferences(map.clone());
```

Commit precedes one infallible publication, while the request keeps its original
binding for validation and response projection.
