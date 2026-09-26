# Storage

The storage layer lives in [`../../crates/hya-store`](../../crates/hya-store)
and persists canonical Events plus narrow auxiliary control state in SQLite.

## Connections

`SessionStore::connect(path)` opens a file-backed SQLite database with:

- create-if-missing enabled
- WAL journal mode
- normal synchronous mode
- five-second busy timeout
- foreign keys enabled
- up to eight pooled connections

`SessionStore::connect_memory()` opens an in-memory SQLite database with one
connection.

CLI session-store selection
([`main.rs`](../../crates/hya-backend/src/main.rs),
[`open_store`](../../crates/hya-app/src/runtime.rs)):

| Surface | Empty / default `--db` | Explicit `--db <PATH>` |
| --- | --- | --- |
| Goal mode (`-p` / `--prompt`), `rpc` | Always in-memory (`connect_memory`) | N/A (no file path used) |
| `exec`, `run`, `serve` | In-memory via `open_store("")` | File-backed at that path |
| `sessions`, `tail-session` | **Not** in-memory: `resolve_interactive_db` remaps empty to `$XDG_STATE_HOME/hya/sessions.db` (fallback `$HOME/.local/state/hya/sessions.db`, then `./.local/state/hya/sessions.db`) and creates the directory | File-backed at the given path |

So the session-backed subcommands default to the durable XDG
state database, not a fresh memory store.

File-backed stores are plain SQLite. They are not encrypted and file permissions
come from the process umask, so callers should place `--db` paths in private
directories when transcripts, tool outputs, commands, or workdir paths are
sensitive.

PRAGMAs (WAL and friends) are set via connect options, not migrations — WAL
cannot run inside the transaction sqlx wraps migrations in.

## Migrations

Session-store migrations live under
[`crates/hya-store/migrations/`](../../crates/hya-store/migrations/). A
**separate** database and migration set for the bundle registry is documented
under [Bundle registry database](#bundle-registry-database).

### `0001_init.sql`

Creates the base relational tables plus the event log and token ledger.
The current runtime read path is **event-log based**. Tables such as
`message` and `part` exist in the schema, but `read_projection` folds from
`event_log` rather than querying materialized message rows.

| Table | Columns | Keys / indexes |
| --- | --- | --- |
| `session` | `id BLOB PK`, `parent_id BLOB` FK → `session(id)`, `agent TEXT NOT NULL`, `model TEXT NOT NULL`, `workdir TEXT NOT NULL`, `title TEXT`, `permission TEXT NOT NULL DEFAULT '{}'`, `created_at`, `updated_at` | Index `session_parent` on `parent_id` |
| `message` | `id BLOB PK`, `session_id` FK → `session(id)` **ON DELETE CASCADE**, `role`, `agent`, `model`, `finish`, `cost_json`, `tokens_json`, `created_at` | Index `message_session` |
| `part` | `id BLOB PK`, `message_id` FK → `message(id)` **ON DELETE CASCADE**, `seq`, `kind`, `body_json` | **UNIQUE** `(message_id, seq)` |
| `event_log` | see [Event Log](#event-log) | |
| `team_run` | `id BLOB PK`, `lead_session` FK → `session(id)`, `spec_json`, `state`, `created_at` | |
| `team_member` | `id BLOB PK`, `team_id` FK → `team_run(id)` **ON DELETE CASCADE**, `session_id` FK → `session(id)`, `background_task_id`, `role`, `state`, `created_at` | |
| `mail` | `id BLOB PK`, `team_id` FK CASCADE, `from_ep`, `to_ep`, `kind`, `body_json`, `delivered_at`, `acked_at`, `created_at` | **Pre-ADR-0001 relational mailbox.** Superseded by event-sourced `MailSent`; not on any live read path. |
| `task_board` | `id BLOB PK`, `team_id` FK CASCADE, `title`, `body`, `status`, `assignee`, `created_at`, `updated_at` | **Pre-ADR-0001.** Not on any live read path. |
| `goal` | `id BLOB PK`, `session_id` FK → `session(id)`, `condition`, `bound_json`, `state`, `turns_evaluated`, `last_reason`, `started_at`, `cleared_at` | |
| `token_ledger` | `id BLOB PK`, `session_id BLOB` (no FK), `turn`, `provider`, `model`, `team_id`, `completion_run_id`, `iteration`, `role`, `category`, `prompt_tokens`, `completion_tokens`, `confidence`, `ts` | |

### `0002_sync_event.sql`

```sql
CREATE TABLE sync_event (
    aggregate_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (aggregate_id, seq)
);
CREATE INDEX sync_event_seq ON sync_event(seq);
```

Backs the Compat `/sync/history` and `/sync/replay` routes. See
[Sync store API](#sync-store-api).

### `0003_saved_permission.sql`

```sql
CREATE TABLE saved_permission (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    action     TEXT NOT NULL,
    resource   TEXT NOT NULL,
    UNIQUE(project_id, action, resource)
);
CREATE INDEX saved_permission_project ON saved_permission(project_id);
```

Durable store for **allow-always** permission decisions. See
[Saved permissions](#saved-permissions). `0013_saved_permission_time.sql`
adds `time_created INTEGER` (ms since the Unix epoch; `NULL` for rows saved
before it).

### `0004` → `0008` admission journal

Spawn-budget durability lives in `admission_journal`. Full schema evolution
and state machine are documented in
[admission-and-governor.md](admission-and-governor.md) so the SQL and the
engine/store API cannot drift across docs.

Summary of migration roles:

| Migration | Role |
| --- | --- |
| `0004_admission_journal.sql` | Initial single-row-per-operation journal (`accepted`…`aborted`) |
| `0005_resident_actor_claim.sql` | Adds `resident_actor_claim` + nullable `actor_id` / `actor_epoch` on the journal |
| `0006_admission_queue_states.sql` | Rebuilds to composite PK `(operation_id, member_ordinal)`, adds `queued` / `waiting`, `batch_size` |
| `0007_admission_bindings.sql` | All-or-nothing binding columns + `spawn_intent` (1..=1 MiB) |
| `0008_admission_fairness.sql` | `admission_sequence` / `promotion_sequence` + FIFO partial indexes |

### `0009_agent_model_preference.sql`

Adds the backend-owned per-Agent model preference table:

| Column | Constraint |
| --- | --- |
| `agent_id` | `TEXT PRIMARY KEY`, length 1..=1024 |
| `provider_id` | `TEXT NOT NULL`, length 1..=1024 |
| `model_id` | `TEXT NOT NULL`, length 1..=4096; provider-local slashes are preserved |

This is an auxiliary control table, not a Session projection. It emits no
public Event. Runtime startup claims the database owner, loads the complete map,
and publishes one immutable snapshot before admission. A mutation validates the
bound Agent and exact provider-catalog row, commits under the matching runtime
owner, then publishes the complete replacement map. Old `TurnBinding`s retain
their captured snapshot; failed or stale-owner writes publish nothing.

Only stable Agent/provider/model identities are stored. Credentials, reasoning
variants, prompts, Session content, and provider responses never enter this
table. File-backed databases retain the rows across restart; memory databases
do not, and separate database paths remain isolated.

### `0011_projection_snapshot.sql`

Adds `projection_snapshot`, the durable level of the
[projection cache](#projection-reads):

| Column | Constraint |
| --- | --- |
| `session_id` | `BLOB PRIMARY KEY` (session storage key) |
| `reducer_version` | `INTEGER NOT NULL` — `hya_proto::PROJECTION_REDUCER_VERSION` that folded the row |
| `last_seq` | `INTEGER NOT NULL` — last `event_log.seq` folded into the snapshot (the anchor) |
| `payload` | `BLOB NOT NULL` — `Projection::encode_snapshot` JSON |

The table starts empty and holds only derived data: dropping every row is
always safe (the next read of each session folds its full log once and writes
a fresh row). Upgrading a database therefore costs one full fold per session,
paid lazily by the first read of that session.

### `0012_file_blob.sql`

Adds `file_blob`, the content store behind session revert
([Runtime — File snapshots and revert](runtime.md#file-snapshots-and-revert))
and, since R4 of 0.41.0, prompt image attachments (no schema change):

| Column | Constraint |
| --- | --- |
| `session_id` | `BLOB NOT NULL` (session storage key) |
| `hash` | `TEXT NOT NULL` — lowercase hex sha256 of `content` |
| `size` | `INTEGER NOT NULL` — `content` length in bytes |
| `content` | `BLOB NOT NULL` — the file bytes |

Primary key `(session_id, hash)`, `WITHOUT ROWID`. Events (`files_changed`,
`session_reverted`, `session_unreverted`) carry only the hash; the content
lives here, stored once per session and hash. It is auxiliary data, not a
projection: the rows are written before the event that names them, and a
missing row only makes that one file unrestorable (the revert reports it as
`failed`).

Store API: `put_file_blob(session, hash, content)` (`INSERT OR IGNORE`),
`file_blob(session, hash) -> Option<Vec<u8>>`,
`file_blob_bytes(session) -> u64` (the engine's per-session cap check), and
`append_events_with_blobs(session, blobs, events)`, which inserts blobs and
appends events in one transaction (an image prompt: its images and its user
message). Prompt images are referenced from `user_prompt_context_recorded`
by hash, so the event log and projection snapshots stay small and replay is
unchanged: the fold never reads blobs; only a model request does.

### `0005_resident_actor_claim.sql` (claim table)

Adds coordination table `resident_actor_claim`:

| Column | Constraint |
| --- | --- |
| `actor_id` | BLOB PK (stable resident session identity) |
| `epoch` | INTEGER NOT NULL, `> 0` |
| `owner_run_id` | BLOB NOT NULL, length 16 |
| `state` | `active` \| `released` |

Index `resident_actor_claim_state` on `(state, actor_id)`.

Claim acquisition, takeover, release, resident event append, and actor-bound
admission transitions use indexed point checks inside SQLite transactions.
Release terminalizes the exact actor/epoch's nonterminal admission rows before
marking the full claim tuple reusable. Startup enumerates active resident
claims once, advances their epochs before runtime readiness, runs the existing
global abort only for non-actor rows, then aborts old actor rows through each
recovered claim. A logical release recorded during restart never credits the
new process's empty in-memory governor.

## Event Log

Schema (`0001`):

| Column | Type | Notes |
| --- | --- | --- |
| `seq` | `INTEGER PRIMARY KEY AUTOINCREMENT` | **Global** across all sessions, not per-session |
| `session_id` | `BLOB NOT NULL` | **No FK** to `session(id)` |
| `payload` | `TEXT NOT NULL` | Serialized `Event` JSON |
| `ts` | `INTEGER NOT NULL` | Unix epoch milliseconds |

Index: `event_log_session` on `session_id`.

Consequences:

1. **Global `seq`** — one session's envelopes have gaps; clients must treat
   sequences as strictly increasing but not contiguous.
2. **No FK to `session`** — an event log can exist with no `session` row. That
   is why `list_sessions` derives identity from the log, not the `session`
   table.

`append_event` inserts:

- session storage key bytes (`hysec_...` ASCII for new sessions; 16-byte UUID
  keys for legacy sessions)
- serialized `Event` JSON
- timestamp in Unix epoch milliseconds

SQLite assigns the global autoincrement `seq`, which becomes `Envelope.seq`.

### Session key encode / decode

| Direction | Rule |
| --- | --- |
| **Write** (`SessionId::storage_key`) | `hysec_...` ASCII bytes for new sessions; 16-byte UUID for legacy |
| **Read** (`decode_session_key`) | If the blob is **valid UTF-8**, parse as `SessionId` text (`hysec_`, `ses_<uuid>`, or raw uuid text) and **return that result** (`Some` or `None`) — **no** binary UUID fallback after a failed parse. Only when the blob is **not** valid UTF-8, interpret exactly 16 bytes as a legacy raw UUID. |

Compatible readers must mirror that control flow: a 16-byte legacy UUID whose
bytes happen to be valid UTF-8 (all `< 0x80`) will **not** be recovered by the
binary branch; production legacy keys are raw UUID bytes (not text), so they
take the non-UTF-8 path.

This is a full replay log, not a rendered transcript cache. Persisted events can
include prompts, tool-call inputs, tool outputs, reasoning deltas, command
metadata, context file paths, absolute workdir paths, and token usage data.

`ResidentWorkStarted` is the only added resident recovery marker. It records
the stable actor session, epoch, handle, and inbox boundary, but no tool output
or external-effect payload. The shared reducer clears it and advances the
durable inbox cursor when the resident reaches an idle/terminal activity state.

`replay(session)` loads all rows for one session ordered by `seq` and deserializes
each payload into an `Envelope`.

## Session list and delete

### `list_sessions` → `SessionInfo`

```text
SELECT session_id, MIN(ts), MAX(ts), COUNT(*)
FROM event_log
GROUP BY session_id
ORDER BY updated DESC, session_id DESC
```

Each row becomes:

| Field | Meaning |
| --- | --- |
| `session` | Decoded via `decode_session_key` |
| `started_millis` | `MIN(ts)` |
| `updated_millis` | `MAX(ts)` |
| `events` | Row count for that session |

Session identity comes from the **log**, not the `session` table — a session
row is not required for a session to be listed.

### `delete_session`

One transaction:

1. `DELETE FROM token_ledger WHERE session_id = ?`
2. `DELETE FROM open_assistant_message WHERE session_id = ?`
3. `DELETE FROM projection_snapshot WHERE session_id = ?`
4. `DELETE FROM file_blob WHERE session_id = ?`
5. `DELETE FROM event_log WHERE session_id = ?`

After the commit the session's in-process cached projection is dropped.
Returns whether any **event_log** rows were removed.

## Projection Reads

`read_projection(session)` returns exactly

```text
Projection::from_events(replay(session))
```

— the shared `hya_proto::Projection` reducer over the session's log — but it
does not decode the whole log on every call. Store replay, HTTP event reads,
SSE recovery, and transcript rendering stay on that one reducer; remote
clients consume the curated v1 stream/read shapes over HTTP+SSE.

### Projection cache

[`projection_cache.rs`](../../crates/hya-store/src/projection_cache.rs) caches
the reducer's output at two levels. Both are a **pure cache** of the event log:
they hold nothing that cannot be rebuilt by a full replay, and no read path
treats them as a source of truth.

| Level | Where | Written | Scope |
| --- | --- | --- | --- |
| In-process | `Arc<Projection>` per session, shared by every clone of one `SessionStore` (least recently read of 256 sessions evicted) | every read that folded new events | one process |
| Durable | `projection_snapshot` row per session ([`0011`](#0011_projection_snapshotsql)) | a read that folded ≥ 1024 events since the last snapshot (every read for the first fold of an existing log above that size) | every process on the database |

A read (`read_projection`, `read_projection_shared`, `with_projection`):

1. takes the in-process projection, else the durable snapshot, as the base;
2. selects `event_log` rows of the session with `seq >= base.last_seq`, checks
   that the first row is the base's anchor (`seq == last_seq`), and applies
   only the rows after it with `Projection::apply`;
3. caches the result and, past the interval, upserts the durable snapshot.

`read_projection_shared` returns the cached `Arc<Projection>` and
`with_projection(session, |p| ...)` borrows it, so hot readers (the `wait`
tool, the steer mailbox, the `session.usage` capability, lineage walks) pay
neither a replay nor a deep clone. A read of an unchanged session costs one
indexed query that returns only the anchor row.

**Invariants** (why cached base + tail equals a full replay):

- `event_log` is append-only per session; the only removal is
  `delete_session`, which drops every row of the session. `seq` is a global
  AUTOINCREMENT assigned under SQLite's single writer, so commit order equals
  `seq` order: once the anchor row is visible, every earlier row of the
  session is too, and every later row has a larger `seq`.
- Every fold re-reads the anchor. When it is gone — the session was deleted
  (possibly by another process), or the database was restored from an older
  copy — the base is discarded and the full log is folded.
- A durable snapshot is used only when its `reducer_version` equals the
  running `PROJECTION_REDUCER_VERSION` and its payload decodes to a
  projection whose `last_seq` matches the row; anything else is ignored and
  overwritten by the next snapshot write. Snapshot writes are anchored in SQL
  (`INSERT … SELECT … WHERE EXISTS (anchor row)`), so a write racing a delete
  is a no-op.
- The snapshot encoding (`Projection::encode_snapshot`) carries replay-only
  reducer state the wire projection omits (the Workflow run dedupe set), so
  decoding a snapshot and folding the tail is indistinguishable from a replay.
- Writer transactions (mail appends, resident recovery, crash recovery,
  Workflow selection) fold through `replay_projection(cache, tx, session)`:
  they start from the cache, check the anchor through the transaction, and
  see their own uncommitted events — so they **never write back** to the
  cache. Startup recovery warms the cache outside the transaction first, so
  the transaction folds only the tail.

**Invalidation.** Bump `hya_proto::PROJECTION_REDUCER_VERSION` whenever
`Projection::apply` can fold the same events differently or the projection's
shape changes. The `reducer_fingerprint_pins_the_version` test in
`crates/hya-proto/tests/projection_snapshot.rs` hashes the reducer's output
over generated logs and fails until the bump (and the new fingerprint) is
recorded. Older snapshots are then ignored and rebuilt lazily. A process
running an older binary against the same database ignores newer snapshots the
same way; the rows are simply rewritten by whichever version reads next.

**Equivalence tests.** `crates/hya-proto/tests/projection_snapshot.rs`
checks `decode(encode(fold(prefix))) + tail == fold(all)` at every split of
generated logs (streaming, usage records and legacy totals, message/part
deletion, compaction markers, forks, re-emitted Workflow run starts, team
traffic). `crates/hya-store/tests/projection_cache.rs` checks warm and
restarted reads against a full replay across interleaved appends, reducer
version changes, undecodable and unanchored snapshots, and deletes; a unit
test checks that a rolled-back transaction fold never reaches the cache.

**Measured effect.** A real 187 MB database (14 sessions, ~646k events, the
root session ~215k events mostly `reasoning_delta`/`tool_input_delta`, 8
resident claims left active by a crash), debug build, same machine:

| Operation | Before (full replay per read) | After |
| --- | --- | --- |
| `hya serve` ready, first open after upgrade (no snapshots yet) | 86 s | 5–6 s |
| `hya serve` ready, restart after a crash (snapshots present) | 81 s | 1.2 s |
| resident recovery phase (`residents_recovered`) | 79 s | 1.9 s first open, 0.6 s restart |
| RSS at readiness | 450 MB | 100 MB |
| token-summary tree usage (14 sessions), repeated | 4.2 s | 4 ms |
| same, first request after a restart | 4.2 s | 0.3 s |
| `GET /v1/sessions` | 8.8 s | 0.3 s |
| root projection read: full fold / from snapshot / warm | 1.3 s / – / – | 1.3 s / 61 ms / <1 ms |

The first read of a session that has no snapshot yet still folds its whole
log once (1.1 s for a 190k-event log in a debug build). Reproduce with
`cargo run -p xtask -- startup-bench --db <copy.db> --timeout-secs 300`
(phase waterfall) and the ignored equivalence bench
`HYA_PROJECTION_CACHE_DB=<db> cargo test -p hya-store --test projection_cache -- --ignored --nocapture`,
which also asserts that every session's cold, snapshot, and warm reads equal
its full replay.

**Tuning.** `SessionStore::with_projection_snapshot_interval(events)` changes
the durable-snapshot interval (tests use `1`). There is no configuration key:
the interval only moves cost between snapshot writes and tail folds, never the
result.

## Materialized team tables

The event log remains the single source of truth; `session`, `team_run`,
`team_member`, `mail`, and `task_board` are queryable write-through
projections maintained inside the same transaction as the event append
(`SessionStore::append_event`, `append_event_in_transaction`, and the
resident-mutation batch):

| Event | Materialized rows |
| --- | --- |
| `session_created` | `session` (authoritative row; `INSERT OR IGNORE`) |
| `agent_registered` | `team_run` ensure (the orchestration root's log session is the run) + `team_member` |
| `mail_sent` | `mail` (`from_ep`/`to_ep`/`kind`/`body_json`, `delivered_at` = append time; `acked_at` stays NULL) |
| `member_spawned` | `task_board` row (`status: pending`) |
| `subagent_reported` | `task_board` status → `done`/`failed` |

FK anchors are self-healing: when a registration arrives on a log whose
`session` row does not exist (synthetic or migrated sequences), a placeholder
`session` row is anchored first and a later authoritative `session_created`
keeps its own values. Reads never consult these tables — replaying
`event_log` through the shared projection stays the only derivation path.

## Token Ledger

The engine records one row per finished assistant message — every turn, every
session kind (root, subagent, resident actor): `SessionEngine::emit` hooks the
assistant `MessageFinished` event, and resident mutations get the same hook in
`commit_resident_mutation`. Recording is best-effort: a ledger failure is
logged and never fails the finished turn.

`record_usage` inserts into `token_ledger` with:

- `session_id` (storage key)
- `iteration`
- `completion_run_id`
- `role` (the session's agent name)
- `prompt_tokens`, `completion_tokens`
- `confidence` — how the numbers were obtained (below)
- `provider`, `model` — the model that served the message's latest attributed
  round (`MessageProjection.usage.model`), else the session's model ref
- `ts` (now)

`read_usage` returns those fields for a session ordered by timestamp.

The ledger is a best-effort side table. The replayable per-model account is
`SessionProjection.usage`, folded from `UsageRecorded` events (see
[event-model.md](event-model.md#session-usage-fold)); a message whose rounds
ran on several models records only the latest round's model here.

### Confidence levels

| `confidence` | Meaning |
| --- | --- |
| `provider` | The provider reported usage on the wire: the sum of the message's `UsageRecorded` rounds (also on a cancelled or errored message), else the legacy `MessageFinished.tokens`. `prompt_tokens` is the whole prompt `input + cache_read + cache_write`, `completion_tokens` is `output` (thinking included). |
| `hf:<repo>` | The provider reported nothing; the turn's texts were counted with the model family's real `tokenizer.json` (GPT, Claude, DeepSeek, GLM, Kimi, Qwen initially adapted; resolved lazily from the hya cache → local HF cache → one-time download, then cached per process). |
| `estimated` | No family matched or no tokenizer resolved; the structure-aware `CalibratedTokenizer` estimate was used. |

The remaining optional columns (`turn`, `team_id`, `category`) are still not
populated by this path.

## Saved permissions

When a permission ask is answered **`always`**, the server writes one durable
row via `SavedPermissions::remember`:

| Field | Value |
| --- | --- |
| `id` | `psv_<requestId>` |
| `project_id` | literal **`"global"`** (not scoped per project or session) |
| `action` | lowercase `Action` name (`tool`, `read`, `edit`, `glob`, `grep`, `bash`, `task`, `mcp`, `webfetch`, `websearch`, `todowrite`, `skill`, `lsp`, `externaldirectory`) |
| `resource` | remembered match pattern string |

**Global scoping means an allow-always granted in one workspace applies in all
of them** for that action/pattern pair (subject to the unique constraint on
`(project_id, action, resource)`).

Store API:

| Method | Behavior |
| --- | --- |
| `save_permission` | `INSERT OR IGNORE` — re-saving is a no-op; stamps `time_created` with now when `time_created_ms` is `None` |
| `list_saved_permissions(project_id: Option<&str>)` | Filter by project or list all |
| `saved_permission(id)` | Read one row by id |
| `remove_saved_permission(id)` | Delete by id |

v1 HTTP (see [Server and Client](server-client.md)):

- `GET /v1/permissions/rules` (saved-rule list, feeds the bootstrap snapshot)
- `DELETE /v1/permissions/rules/{rule}`

Rows survive server restart because they live in the session SQLite file, and
so do the grants: at startup `AppState::restore_saved_permissions` reads every
row into the process `PermissionPlane` (`persistent` rules for `*` rows,
`native_grants` for exact `tool` / `mcp` / `bash` subjects), and
`DELETE /v1/permissions/rules/{rule}` revokes the in-memory grant along with
the row. See [Tools and permissions — Saved
grants](tools-and-permissions.md#saved-grants).

## Sync store API

Alongside the `sync_event` table:

### `replay_sync_events(events: &[Value]) → Vec<Value>`

For each event with camelCase `aggregateID` and `seq`, runs
`INSERT OR IGNORE INTO sync_event (aggregate_id, seq, payload)`. The stored
payload is reshaped by `history_event` to snake_case keys
`{ id, aggregate_id, seq, type, data }`. The return value is **not** that
stored shape: it is a clone of each **caller-supplied** event that was newly
inserted (still camelCase `aggregateID`, etc.). A re-replay of an overlapping
history returns an empty set for those rows.

### `sync_history(known: &BTreeMap<String, u64>) → Vec<Value>`

Returns every stored event (ordered by `seq`) whose sequence is **strictly
greater** than the caller's per-aggregate `known` watermark (or any event for
aggregates absent from `known`). Each element is the **stored** JSON payload
(`{ id, aggregate_id, seq, type, data }` — snake_case). Clients must not expect
`aggregateID` on this path.

## Bundle registry database

A **second** SQLite file, separate from the session/event database. Schema and
migrations live under
[`crates/hya-store/bundle_migrations/`](../../crates/hya-store/bundle_migrations/).
(CLI path for the file is documented in `docs/cli.md` —
`$XDG_DATA_HOME/hya/bundles/registry.sqlite3`.)

### Schema (`bundle_migrations/0001_init.sql`)

| Table | Columns |
| --- | --- |
| `bundle_registry_generation` | `singleton INTEGER PRIMARY KEY CHECK (singleton = 1)`, `generation INTEGER NOT NULL CHECK (generation >= 0)` — seeded at `0` |
| `installed_bundle` | `bundle_id TEXT PK`, `version`, `publisher`, `source_digest BLOB(32)`, `prepared_digest TEXT`, `prepared_bytes BLOB`, `installed_at INTEGER` |

### Connection PRAGMAs (deliberately different from the session store)

| Setting | Session store | Bundle registry |
| --- | --- | --- |
| Journal | WAL | WAL |
| Synchronous | **Normal** | **Full** |
| Busy timeout | **5 seconds** | **Zero** (contention fails fast as `StoreError::BundleRegistryBusy`) |
| Pool max | 8 | 8 |
| Foreign keys | on | on |

### API (`BundleRegistry`)

| Method | Result |
| --- | --- |
| `generation()` | Current registry generation |
| `snapshot()` | `BundleRegistrySnapshot { generation, bundles: Vec<BundleRegistryRecord> }` |
| `install_inspection(...)` | Install from a package inspection (public only; private → `PrivateActivationUnsupported`) |
| `install(...)` | `BundleInstallOutcome`: `Installed` / `Replaced` / `Unchanged` (each with generation) |
| `uninstall(...)` | `BundleUninstallOutcome::Removed { generation }` |

`BundleRegistryRecord` fields: `bundle_id`, `version`, `publisher`,
`source_digest`, `prepared_digest`, `prepared_bytes`, `installed_at`.

## Errors

`StoreError` variants
([`error.rs`](../../crates/hya-store/src/error.rs)):

| Variant | When |
| --- | --- |
| `Sqlite` | Underlying sqlx/SQLite error |
| `Migrate` | Migration runner failure |
| `Json` | Event/payload JSON (de)serialization |
| `Bundle` | `hya-bundle` prepare/catalog error |
| `BundleRegistryData` | Invalid registry generation or install candidate shape |
| `BundleRegistryCorrupt` | Stored prepared catalog bytes fail decode/validation |
| `BundleRegistryBusy` | Writer contention with zero busy timeout |
| `BundleNotFound` | Uninstall/lookup of missing bundle |
| `BundleContentConflict` | Same version, different content on install |
| `PrivateActivationUnsupported` | Private package inspection cannot be installed |
| `BundleAgentIdReserved` | Bundle declares a reserved built-in agent id |
| `OperationIdConflict` | Immutable admission claim fields differ for the same operation id |
| `AdmissionNotFound` | No journal row for the operation |
| `AdmissionTransitionConflict` | Illegal state transition |
| `AdmissionData` | Journal invariant / units / intent size / unknown state |
| `AdmissionCapacityExceeded` | Active or non-active caps exceeded |
| `ActorAlreadyClaimed` | Resident actor claim held by another owner |
| `StaleActorClaim` | Epoch/owner no longer current |
| `ActorClaimUnavailable` | No recoverable active claim |
| `ActorClaimData` | Claim payload/data error |
| `RuntimeOwnerBusy` | Exclusive runtime-owner lock already held |
| `RuntimeOwnerClaimRequired` | Matching runtime owner claim required for recovery |
| `RuntimeOwnerLock` | Runtime-owner lock file I/O failed |
| `WorkflowData` | Malformed or inconsistent Workflow control mutation |
| `MailboxRejected` | Mailbox write rejected |

## Replay Surfaces

The same store replay powers:

- `SessionEngine::replay`
- `GET /v1/sessions/{session}/events` (curated replay; `include_raw` returns
  the raw envelope lines)
- `hya tail-session`
- `read_projection` (through the [projection cache](#projection-cache), which
  folds the same log with the same reducer)

This makes the database a useful debugging artifact: if the event log is intact,
the session can be reconstructed.
