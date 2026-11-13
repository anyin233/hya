# Database Guidelines

> Database patterns and conventions for this project.

---

## Overview

- `hya-store` uses `sqlx` with SQLite. `SessionStore` owns the connection pool
  and runs embedded migrations during `connect`/`connect_memory`.
- The append-only `event_log` plus `hya_proto::Projection` is canonical for
  session behavior. Auxiliary tables must not become a second session
  projection or emit parallel public events.
- Active `resident_actor_claim` rows are the live-member capacity leases.
  Registration and revival check the root's indexed count in their writer
  transaction; terminal archive releases its claim in the same transaction.
- Exception: `BundleRegistry` owns the separate
  `<data_root>/bundles/registry.sqlite3` installed-package control-plane DB;
  builtins are not rows, and it is not a session projection.
- One server per session database (ADR-0022, ADR-0023): `hya serve` (in the
  foreground, or as the backend daemon `hya serve start` / bare `hya` / the
  TUI start) takes `<db>.runtime-owner.lock` in `hya-store` before SQLite
  initialization and publishes `<db>.server.json` once listening. Frontends
  attach to that server; they never open a served database themselves. Every
  writer of a session database claims the same lock through
  `crates/hya-backend/src/db_writer.rs`: headless commands (`exec`/`run`,
  `workflow use|run|state`, `sessions archive|unarchive`) hold it for their
  run, or go through the owning server over `/v1`
  (`crates/hya-backend/src/routed.rs`), or exit 75. Read-only commands
  (`sessions` listing, `tail-session`) never write projection snapshots and
  do not lock.

---

## Query Patterns

- Prefer conditional single-statement transitions (`UPDATE ... WHERE state =
  ... RETURNING`) for compare-and-set state changes.
- Resident actors use one indexed `resident_actor_claim` row keyed by their
  persisted agent-session `SessionId`. Claim/recover/release are transactional
  compare-and-set operations over the full actor/epoch/owner tuple.
- Resident canonical mutations validate the current claim in the same SQLite
  transaction as event append. Event-bus publication
  occurs only after commit.
- Root turns do not reset live capacity; startup fences active claims without
  releasing them. No connection or transaction spans a hook/model await.
- Runtime file databases require plain paths so relative, absolute and symlink
  spellings use one canonical runtime-owner lock before SQLite opens.
  `database_paths` rejects persistent URI spellings; explicit in-memory URIs
  remain unlocked. See the [CLI ownership contract](../../cli.md#database-lock-and-the-backend-daemon).

---

## Migrations

- The session store uses one `0001_init.sql` baseline under
  `crates/hya-store/migrations/`. Existing schemas without its checksum fail at
  open with `StoreError::UnsupportedDatabase`; never upgrade the old baseline.
- `BundleRegistry` embeds its separate migrations from
  `crates/hya-store/bundle_migrations`; migration tests use a `BundleRegistry`
  temp DB.
- Keep `session` as the placement/parent index. Capacity belongs to active
  claims, indexed by `(root_session_id, state)`.
- Keep CHECK/UNIQUE constraints aligned with Rust invariants; exercise
  `SessionStore` migrations through `SessionStore::connect_memory` tests and
  `BundleRegistry` migrations through its temp-DB tests.
- Actor epoch is monotonic integer state. The claim table must not acquire TTL,
  heartbeat, wall-clock, or background-expiry columns.
- Write-through index tables (`materialize.rs`) are maintained inside the
  event-append transaction and read only for narrow queries that must not
  replay every log. `open_assistant_message` indexes assistant messages
  without a finish for startup crash recovery; every `event_log` writer must go
  through `append_event` / `append_event_in_transaction` /
  `commit_resident_mutation` so the index stays exact, and `delete_session`
  clears its rows.
- `projection_snapshot` is the durable level of the projection cache —
  a pure cache of the shared reducer, never read as truth. Rows are trusted
  only under the running `PROJECTION_REDUCER_VERSION` and while their anchor
  event (`event_log.seq = last_seq` of the same session) exists. Any change to
  `Projection::apply` results or to the projection's shape bumps
  `PROJECTION_REDUCER_VERSION` (the fingerprint test enforces it). Writer
  transactions fold via `replay_projection(cache, tx, session)` and never write
  the cache back; a new session-row removal path must also delete the
  session's `projection_snapshot` row and drop the in-process entry, as
  `delete_session` does. See `docs/architecture/storage.md#projection-cache`.
- `saved_permission.time_created` is a nullable ms timestamp; NULL reports no time.
- `project` / `project_root` (ADR-0024) are mutable configuration
  tables like `saved_permission`, not events and not a projection. A session's
  membership lives in its `session_created` event (`project`, `kind`) and is
  mirrored by the materializer into `session.project_id` / `session.kind`
  in the baseline schema. Session
  existence and archive state still come from the log and the projection:
  Project session counts and `list_sessions_in` join `session` against
  `event_log`, and `delete_project` folds candidate roots to skip archived
  ones. `session.project_id` has no FK, because the log keeps the id after a
  Project is deleted. Roots are validated in Rust (absolute, lexically
  normalized, no `..`, de-duplicated, non-empty) with typed `StoreError`
  variants; `resolve_project_by_path` matches whole path components.
- `file_blob` holds the per-session, content-addressed file contents
  behind session revert (`files_changed` / `session_reverted` events carry
  only the sha256 hash). It is auxiliary content, not a projection: write the
  blob before appending the event that names it, keep it scoped to the
  session (`delete_session` removes the rows), and cap growth in the engine
  (per-file and per-session limits in `hya-core` `file_snapshot`).

---

## Naming Conventions

- Use snake_case table, column, and index names.
- UUID-backed operation/tool-call keys are 16-byte BLOBs; `SessionId` uses
  `SessionId::storage_key()` because new session IDs are not UUID-only.

---

## Common Mistakes

- Never reset capacity at root turn end or maintain a parallel in-memory ledger.
- Never retry work that crossed `ResidentWorkStarted` or conflate actor epoch
  with runtime configuration generation.
- Actor transitions require the matching claim, except explicit orphan takeover.

---

## Scenario: Owner-Fenced Per-Agent Model Preferences

### 1. Scope / Trigger

- Trigger: changing Agent model persistence, runtime binding, root/subagent/
  Workflow/fixed-Agent model resolution, or the agent-model preference
  control.
- The preference table is backend control state. It is not a Session projection
  and must not emit a public `Event`.
- The former HTTP surface (`GET`/`PUT /tui/agent-models`) was deleted with the
  legacy routes; the `hya.v1` contract currently exposes no
  agent-model-preference rpc, so the store/app control below is not reachable
  over HTTP until one is added.

### 2. Signatures

- Migration: `agent_model_preference(agent_id TEXT PRIMARY KEY, provider_id
  TEXT, model_id TEXT)` with length checks `1..=1024`, `1..=1024`, and
  `1..=4096`.
- Store: `list_agent_model_preferences()`,
  `upsert_agent_model_preference(owner, row)`, and
  `remove_agent_model_preference(owner, agent)`.
- App: `PersistentAgentModelControl::set(binding, agent_id, identity)` and
  `effective_model(binding, agent_id, base_model)`.
- Server: none today. The deleted legacy surface was `GET /tui/agent-models`;
  `PUT /tui/agent-models/:agent_id` with
  `{ "preference": { "providerID", "modelID" } }` or
  `{ "preference": null }`. A v1 equivalent must be designed as a
  `hya.v1` rpc before any new HTTP exposure.

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
- Publishing a preference never rewrites the model recorded by an existing
  Session or mutates its replay. A deliberate client selection change affects
  an open Session's next turn by sending the backend-committed effective
  identity as request-local prompt state; a preference write alone is not a
  Session mutation.
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

| Condition | Result (as defined by the deleted HTTP surface; re-derive for any future v1 rpc) |
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
- Server tests (historical): the deleted route's bootstrap/list/set/clear,
  model-local-slash, one-binding root creation, exact 400/404/409/503 bodies,
  and empty-control assertions were retired with it; no v1 suite covers this
  control today.
- Real process tests (historical): the deleted surface's targeted-Agent B /
  untouched-Agent A / restart / clear / stale-catalog fallback comparisons were
  retired with it. A normal-picker process test must cover the separate
  open-Session request-local switch; never encode it as a global mutation
  rewriting Session replay.

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

Resident creation uses `register_episodes`: the whole batch's child sessions,
parent member relations, roster/channels, claims and optional directive `MailSent`
commit with lazy root registration and a live-capacity check. Failed registration
leaves none of these rows/events behind. The directive names the parent–child
DM and direct parent sender. Recovery identifies it as ordinary unread mail.
Parked registrations omit mail; they have no initial user message.

`SessionStore::begin_transaction()` acquires SQLite's writer reservation with
`BEGIN IMMEDIATE`, before any read. Registration resolves an existing group
channel for the same root/unit inside that transaction, so concurrent siblings
join one broadcast channel. Publication uses returned committed envelopes.

Wake hooks run before `begin_work`, outside SQL and locks. A fresh wake admits
one coalesced user message without reading the child projection in its writer
transaction. `ResidentWorkPrompt::Resume` is reserved for user-cancelled admitted
work. Stopping before first admission leaves directive mail in the handoff's
Pending section; no synthetic assistant cancellation answer is written.
Outgoing DM copies stay in channel history and the sender inbox, but cannot wake
the sender or prevent its report. Report checks ignore those copies both before
handoff and in the terminal writer transaction.

`finalize_resident_archive(owner: ResidentArchiveOwner<'_>, root: SessionId,
handle: &str, archive: ResidentArchive)` returns
`Result<(ResidentArchiveOutcome, Vec<Envelope>), StoreError>`.
`Claim(&ActorClaim)` fences the episode; `Takeover` requires the store's registered
runtime owner and bumps an orphan's active claim epoch under that owner inside
the terminal transaction. The caller guarantees no in-memory episode exists.
For example, after an archive write fails, retry the orphan with `Takeover` and
`check_gate: false`; if another ending already released and archived the same
actor under the same root/handle, the retry returns `Committed` with an empty
event list. A missing claim, or a released claim without the matching
root/handle/actor archive, returns `ActorClaimUnavailable`. A store that has not
claimed runtime ownership returns `RuntimeOwnerClaimRequired`.
`Takeover` with `check_gate: true` returns `ActorClaimData` ("voluntary report
requires an episode claim"): voluntary report gates belong to live episodes.
Claim-based gate refusals commit only a harness notice and return `Refused`;
terminal effects return `Committed`. Refused or failed transitions leave the
claim tuple unchanged. A store error rolls back everything, including takeover.
The engine supplies
the prepared handoff, archive reason, terminal note and `member_status` explicitly
(see [Task Tool](task-tool.md#atomic-resident-termination)); the store never
synthesizes a handoff. Turn owners close running assistant messages with their
ending cause before finalization. Returned envelopes
are published after commit. Handoff, optional report/mail, archive and claim
release share that transaction; a failed transition has no fallback writes.
Terminal member closure also replaces the previous episode's result when a
revived actor stops before its first turn. The claim/archive fence makes repeat
finalization idempotent. Revival commits its prior-handoff message and initial
directive together with the new claim, so rejected registration writes nothing.
