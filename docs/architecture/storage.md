# Storage

The storage layer lives in [`../../crates/hya-store`](../../crates/hya-store)
and persists canonical events in SQLite.

## Connections

`SessionStore::connect(path)` opens a file-backed SQLite database with:

- create-if-missing enabled
- WAL journal mode
- normal synchronous mode
- five-second busy timeout
- foreign keys enabled
- up to eight pooled connections

`SessionStore::connect_memory()` opens an in-memory SQLite database with one
connection. The CLI uses in-memory stores for goal mode and `rpc`; `exec`,
`run`, the TUI, `serve`, `tail-session`, and `sessions` use file-backed SQLite
when `--db <PATH>` is supplied, otherwise they use in-memory stores where the
command supports an empty database path.

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
[Saved permissions](#saved-permissions).

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
| **Read** (`decode_session_key`) | First try UTF-8 parse as `SessionId` (`hysec_`, `ses_<uuid>`, or raw uuid text); **only if that fails**, interpret the 16 bytes as a legacy raw UUID |

That ordering is what lets both encodings coexist in one `session_id` column.

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
2. `DELETE FROM event_log WHERE session_id = ?`

Returns whether any **event_log** rows were removed.

## Projection Reads

`read_projection(session)` is intentionally simple:

```text
replay(session) -> Projection::from_events(envelopes)
```

This keeps store replay, HTTP event reads, SSE recovery, transcript rendering,
and TUI state on the same reducer semantics.

## Token Ledger

`record_usage` inserts into `token_ledger` with:

- `session_id` (storage key)
- `iteration`
- `completion_run_id`
- `role`
- `prompt_tokens`
- `completion_tokens`
- `confidence`
- `ts` (now)

`read_usage` returns those fields for a session ordered by timestamp.

The table also has optional columns (`turn`, `provider`, `model`, `team_id`,
`category`) that the current `record_usage` path does not populate.

Provider HTTP routes advertise `usage_reporting: true` in default
`Capabilities` and extract usage from protocol streams when present. Ledger
writes still depend on callers invoking `record_usage`.

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
| `save_permission` | `INSERT OR IGNORE` — re-saving is a no-op |
| `list_saved_permissions(project_id: Option<&str>)` | Filter by project or list all |
| `remove_saved_permission(id)` | Delete by id |

Compat HTTP:

- `GET /api/permission/saved` (optional `projectID` query)
- `DELETE /api/permission/saved/:id`

Rows survive server restart because they live in the session SQLite file.

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
| `BundleImmutable` | Attempt to mutate a builtin bundle id |
| `OperationIdConflict` | Immutable admission claim fields differ for the same operation id |
| `AdmissionNotFound` | No journal row for the operation |
| `AdmissionTransitionConflict` | Illegal state transition |
| `AdmissionData` | Journal invariant / units / intent size / unknown state |
| `AdmissionCapacityExceeded` | Active or non-active caps exceeded |
| `ActorAlreadyClaimed` | Resident actor claim held by another owner |
| `StaleActorClaim` | Epoch/owner no longer current |
| `ActorClaimUnavailable` | No recoverable active claim |
| `ActorClaimData` | Claim payload/data error |
| `MailboxRejected` | Mailbox write rejected |

## Replay Surfaces

The same store replay powers:

- `SessionEngine::replay`
- `GET /sessions/:id/events`
- `hya-backend tail-session`
- `read_projection`

This makes the database a useful debugging artifact: if the event log is intact,
the session can be reconstructed.
