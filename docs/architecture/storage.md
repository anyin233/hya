# Storage

The storage layer lives in [`../../crates/hya-store`](../../crates/hya-store)
and persists canonical Events plus narrow auxiliary control state in SQLite.

## Event append transactions

`SessionStore::append_events(session: SessionId, events: Vec<Event>)` consumes
an ordered batch and returns `Result<Vec<Envelope>, StoreError>` after one
transaction commits. Ordinary user messages append their existing five events
in one batch. Completed text boundaries batch `TextStart`, `TextReplace`, and
`TextEnd`; completed reasoning boundaries batch `ReasoningReplace` and
`ReasoningEnd` (the reasoning start remains durable when received).
Live stream deltas and event ordering are unchanged.

All event append paths use
`append_event_in_transaction(&mut Transaction<Sqlite>, SessionId, Event)`:
the sole row writer serializes the event, inserts `event_log`, updates its
materialized indexes, and returns `Result<Envelope, StoreError>`.
`SessionStore::begin_transaction()` returns a transaction for composing that
writer with auxiliary rows. For example, begin a transaction, append an event,
write its auxiliary row using the same transaction, then commit and publish the
returned envelope. Never publish before commit; dropping or rolling back the
transaction discards both writes.

Resident batches validate the actor claim within the transaction; attachment
batches store their blobs within it. Both consume `Vec<Event>` and return
committed envelopes. Restart cancellation and its pending resume row reuse the
same row writer. The engine publishes committed batches through
`publish_envelope`, including hooks and live subscribers.

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

Runtime composition uses `SessionStore::connect_runtime(path)`: it reserves the
owner lock before any SQLite connection or migration. Read-only CLI commands use
`SessionStore::connect_read_only(path)` instead: the file must exist, migrations
never run, and projection reads cannot write snapshots.

CLI session-store selection
([`main.rs`](../../crates/hya-backend/src/main.rs),
[`open_store`](../../crates/hya-app/src/runtime.rs)):

| Surface | Empty / default `--db` | Explicit `--db <PATH>` |
| --- | --- | --- |
| Goal mode (`-p` / `--prompt`), `loop` | Always in-memory (`connect_memory`) | N/A (no file path used) |
| `exec`, `serve` | In-memory via `open_store("")` | File-backed at that path |
| bare `hya`, `serve start\|status\|stop\|restart`, `sessions`, `tail-session` | **Not** in-memory: `resolve_interactive_db` remaps empty to `$XDG_STATE_HOME/hya/sessions.db` (fallback `$HOME/.local/state/hya/sessions.db`, then `./.local/state/hya/sessions.db`) and creates the directory | File-backed at the given path |

So the session-backed subcommands default to the durable XDG
state database, not a fresh memory store.

File-backed stores are plain SQLite. They are not encrypted and file permissions
come from the process umask, so callers should place `--db` paths in private
directories when transcripts, tool outputs, commands, or workdir paths are
sensitive.

PRAGMAs (WAL and friends) are set via connect options, not migrations — WAL
cannot run inside the transaction sqlx wraps migrations in.

## Session schema and compatibility

Session stores use one baseline,
[`0001_init.sql`](../../crates/hya-store/migrations/0001_init.sql), containing
only the current event log, indexes, and control tables. There is no upgrade
path from the previous schema. Removing the retired relational transcript,
team, mailbox, goal, sync, and durable admission queue tables keeps the schema
aligned with the event-log read path.

`SessionStore::connect(path) -> Result<SessionStore, StoreError>` initializes
an empty database or opens a database with the current baseline checksum in
`_sqlx_migrations`. An existing schema without that baseline fails before
schema initialization with `StoreError::UnsupportedDatabase` and the exact
message:

```text
this database was created by an older version and is no longer supported; move it away
```

Old schema/data are never migrated or decoded. Unknown migration versions are
also rejected by the migration runner; missing versions are not ignored.
`connect_memory()` creates the same baseline for a fresh in-memory store.
The [bundle registry](#bundle-registry-database) has its own database and
migration history, which this session-schema reset does not change.

To start fresh after this error, stop processes using the old database, move
it to a different path, then restart with the original path. For example:

```sh
hya serve stop --db ./sessions.db
mv ./sessions.db ./sessions.db.old
hya serve start --db ./sessions.db
```

If `sessions.db-wal` or `sessions.db-shm` remain after shutdown, move them
alongside the old database before restarting. The old data stay at the moved
path; this version cannot reopen them. A different fresh `--db` path works too.

### Tables

The baseline SQL defines every column, constraint, and index. The current
session database contains these application tables (plus sqlx's migration
history and SQLite's sequence table):

| Table | Contract |
| --- | --- |
| `session` | Placement/parent index: `id BLOB PK`, `parent_id BLOB` FK → `session(id)`, `agent`, `model`, `workdir` TEXT NOT NULL, nullable `title TEXT`, `permission TEXT NOT NULL DEFAULT '{}'`, `created_at`/`updated_at INTEGER NOT NULL`, nullable `project_id TEXT`, `kind TEXT NOT NULL DEFAULT 'project'` (`project` or `temporary`). Indexed by parent and Project. Only `session_created` inserts rows. |
| `event_log` | Canonical Events; see [Event log](#event-log). |
| `saved_permission` | `id TEXT PK`, `project_id`, `action`, `resource` TEXT NOT NULL, nullable `time_created INTEGER` (unix ms); unique `(project_id, action, resource)`, indexed by Project. |
| `resident_actor_claim` | Actor/epoch/owner fence; see [Resident actor claims](#resident-actor-claims). |
| `agent_model_preference` | Per-Agent model identities; see below. |
| `model_effort_preference` | `(provider_id TEXT, model_id TEXT)` PK, `effort TEXT NOT NULL`, `updated_at INTEGER NOT NULL`. |
| `agent_effort_preference` | `agent_id TEXT NOT NULL PK`, `effort TEXT NOT NULL`, `updated_at INTEGER NOT NULL`. |
| `open_assistant_message` | `(session_id BLOB, message_id TEXT)` PK, both NOT NULL, WITHOUT ROWID. The event-append transaction inserts assistant starts and deletes finishes/deletions; startup recovery reads this index. |
| `projection_snapshot` | Derived reducer cache; see below. |
| `file_blob` | Per-session content-addressed bytes; see below. |
| `project`, `project_root` | Mutable Project configuration; see below. |
| `pending_resume` | Event-derived restore index: `session_id BLOB PRIMARY KEY`, `message_id TEXT NOT NULL` (interrupted assistant), nullable `prompt_message_id BLOB` (injected user message UUID), `created_at INTEGER NOT NULL`. A shutdown finish and its row commit together for non-archived roots. Archive eligibility includes every event after a persisted snapshot. Prompt injection records its message id atomically; any assistant start deletes the row. |

The single `append_event_in_transaction` writer upserts this index only for
`MessageFinished { role: Assistant, finish: Cancelled, cause: Some(Shutdown) }`
on a non-archived root. It deletes the session's row unconditionally on
`MessageStarted { role: Assistant }`, including turns outside server restore.
`list_pending_resumes() -> Vec<PendingResume>` returns
`PendingResume { session: SessionId, message: MessageId }` in creation order.
`take_restart_resume(session: SessionId, owner: OwnerRunId) -> Option<(MessageId, Vec<Envelope>)>`
builds prompt text from the indexed message's non-`Completed` tool parts and
the shutdown-interrupted subagents identified by the roster, then appends
`MessageStarted(User)`, `TextStart`, `TextDelta`, `TextEnd`, and
`MessageFinished(User, Stop)` and records `prompt_message_id` in one
transaction, returning the prompt's id with the appended envelopes. An already
injected prompt returns its id with no envelopes so startup can still start its
turn after a crash. Missing rows return `None`; missing, archived, or child
sessions, and an injected prompt no longer among the visible messages (reverted),
have their rows deleted and also return `None`.
The first assistant `MessageStarted` deletes the row in the same transaction.
Another shutdown cancellation inserts a fresh unmarked row: each interruption
gets one prompt. Session deletion also removes the row. Permission/question
requests live only in memory and are cancelled with their turn.
These async methods return `Result<_, StoreError>`; prompt injection requires
the runtime-owner claim. No prompt text is stored at stop.

### Agent model preferences

The backend-owned per-Agent model preference table:

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

### Projection snapshots

`projection_snapshot` is the durable level of the
[projection cache](#projection-reads):

| Column | Constraint |
| --- | --- |
| `session_id` | `BLOB PRIMARY KEY` (session storage key) |
| `reducer_version` | `INTEGER NOT NULL` — `hya_proto::PROJECTION_REDUCER_VERSION` that folded the row |
| `last_seq` | `INTEGER NOT NULL` — last `event_log.seq` folded into the snapshot (the anchor) |
| `payload` | `BLOB NOT NULL` — `Projection::encode_snapshot` JSON |

The table starts empty and holds only derived data: dropping every row is
always safe (the next read of each session folds its full log once and writes
a fresh row). The first read of an existing log without a snapshot pays one full fold.

### File blobs

`file_blob` is the content store behind session revert
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

### Projects

Projects (ADR-0024) are a named, ordered, non-empty list of absolute
workspace roots on the backend machine. They are mutable configuration
managed with CRUD like `saved_permission`, never events; a session names its
Project in `session_created` (see [Event model](event-model.md)), and the
materializer mirrors that on the `session` row.

| Table / column | Definition |
| --- | --- |
| `project` | `id TEXT PK` (`prj_<uuid-simple>`), `name TEXT NOT NULL` (non-blank), `created_at`, `updated_at` (unix ms), `archived INTEGER NOT NULL DEFAULT 0` (0/1) |
| `project_root` | `project_id TEXT NOT NULL` FK → `project(id)` **ON DELETE CASCADE**, `path TEXT NOT NULL`, `ord INTEGER NOT NULL` (`0` = primary root); PK `(project_id, ord)`, **UNIQUE** `(project_id, path)` |
| `session.project_id` | `TEXT NULL`, no FK (the event log keeps the id after its Project is deleted); index `session_project` |
| `session.kind` | `TEXT NOT NULL DEFAULT 'project'`, `project` \| `temporary` |

Store API ([`project.rs`](../../crates/hya-store/src/project.rs)):

| Method | Behavior |
| --- | --- |
| `create_project(name, roots) -> Project` | Validates the name and roots, assigns a fresh `ProjectId` |
| `get_project(id) -> Option<Project>` | One Project, archived or not, roots in order |
| `list_projects() -> Vec<ProjectSummary>` | Non-archived Projects, most recently updated first; `session_count` = root sessions of the Project that still have an event log (archived ones included, subagent sessions not) |
| `rename_project(id, name) -> Project` | Bumps `updated_at` (strictly increasing) |
| `replace_project_roots(id, roots) -> Project` | Replaces the whole ordered list; bumps `updated_at`. Running sessions see the new roots from their next turn |
| `update_project(id, name?, roots?) -> Project` | Rename and/or replace the roots in one transaction (both inputs validated first; all or nothing); bumps `updated_at` when either is given. `rename_project` / `replace_project_roots` delegate to it |
| `project_session_count(id) -> u64` | The `session_count` of one Project, as `list_projects` computes it |
| `delete_project(id) -> bool` | Deletes the Project and (by cascade) its roots; `false` when absent. Refused with `ProjectInUse` while a non-archived root session with an event log belongs to it. Also deletes the Project's saved permissions (`saved_permission.project_id`) in the same transaction. |
| `resolve_project_by_path(path) -> Option<Project>` | The non-archived Project with a root that contains `path`, component-wise (`/a/b` contains `/a/b/c`, not `/a/bc`). Longest matching root wins; among equal lengths, the most recently updated Project |
| `ensure_project_for_path(path) -> (Project, created: bool)` | `resolve_project_by_path(path)`'s Project when one contains `path`, else a newly created Project named after `path`'s last component (`path` itself when it has none, e.g. `/`) with `path` as its only root. One `BEGIN IMMEDIATE` transaction: the lookup and the create-if-missing insert are atomic, so concurrent callers for the same (or a containing) path always converge on one Project rather than racing to create duplicates. Mirrors the semantics of the server's `EnsureProjectForPath` (`crates/hya-server/src/v1/project.rs`), which currently serializes callers with a process-wide lock in front of `resolve_project_by_path` + `create_project`; this store-level method is not yet wired in to replace that lock |
| `list_sessions_in(project: Option<ProjectId>)` | `list_sessions`, narrowed to sessions (root and subagent) whose `session_created` named the Project; `None` lists all |
| `normalize_project_path(path)` (free fn) | Root and resolve-path normalization, below |

Roots are normalized lexically, never canonicalized, and need not exist: the
path must be absolute; `.` components and repeated or trailing separators are
dropped; a `..` component is rejected (`ProjectRootInvalid`), because resolving
it lexically can disagree with the filesystem when a symlink precedes it.
Duplicates are removed keeping the first occurrence. An empty list is
`ProjectRootsEmpty`, a relative path `ProjectRootNotAbsolute`, a blank name
`ProjectNameEmpty`.

### Resident actor claims

Coordination table `resident_actor_claim`:

| Column | Constraint |
| --- | --- |
| `actor_id` | BLOB PK (stable resident session identity) |
| `root_session_id` | BLOB NOT NULL (owning team root; active claim is its capacity lease) |
| `epoch` | INTEGER NOT NULL, `> 0` |
| `owner_run_id` | BLOB NOT NULL, length 16 |
| `state` | `active` \| `released` |

Index `resident_actor_claim_state` on `(state, actor_id)`.

Claim acquisition, takeover, release and resident event append use indexed
checks inside SQLite writer transactions. The active claims for a root are its
live capacity leases, indexed by `(root_session_id, state)`. Terminal archive
releases a lease with its canonical events. Startup advances active claims'
epochs without changing their roots or capacity; root turns do not reset it.

`register_episodes` commits the whole batch's child creation, member/roster/channel
events, claims and optional first-DM directive mail together, including main
registration if absent. It checks live capacity under the writer reservation, as does revival.
See [capacity and exact store signatures](admission-and-governor.md).
`begin_transaction()` uses `BEGIN IMMEDIATE`; group-channel
lookup-or-create for a unit is performed under that reservation. Hooks and
envelope publication run after commit. Stop/failure finalization leaves
unadmitted directive mail unread; the handoff includes it as Pending. It adds no synthetic assistant answer. See the
[store signatures and cancellation causes](../spec/backend/database-guidelines.md)
and [registration usage](../spec/backend/task-tool.md#scenario-atomic-resident-registration-and-arming).

`revive_episode` rechecks mail authorization, reactivates the released claim,
registers the member, records `AgentRestarted`, writes the child's handoff/context
boundary and appends the first mail on the existing DM in one `BEGIN IMMEDIATE`.
It reads the latest handoff inside this transaction and returns the new claim,
inbox cursor and committed envelopes. Concurrent senders that find the member
live append ordinary mail without claiming again. Any failure rolls back all
revival effects. See [the exact inputs, outputs and usage](subagent-orchestration.md#4-revival).

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

- session storage key bytes (`hysec_[A-Za-z0-9]{20}` ASCII)
- serialized `Event` JSON
- timestamp in Unix epoch milliseconds

SQLite assigns the global autoincrement `seq`, which becomes `Envelope.seq`.

### Session key encode / decode

| Direction | Rule |
| --- | --- |
| **Write** (`SessionId::storage_key`) | ASCII bytes of `hysec_[A-Za-z0-9]{20}` |
| **Read** (`decode_session_key`) | Validate UTF-8 and parse the same `hysec_` grammar; return `Result<SessionId, StoreError>`. Invalid keys fail with `SessionKeyData`, including the key and parse error. |

UUID text and binary UUID keys are unsupported. Listing, recovery, and pending
interaction reads propagate malformed keys instead of dropping rows or turning
them into sessions with no owner. Event JSON failures return `EventDecode {
session: SessionId, seq: i64, source: Arc<serde_json::Error> }`; for example, a
row tagged `future_event` fails replay with its session, sequence and unknown
tag. No migration or dual-format decoder is provided. Snapshot reducer version
**1** defines the new baseline.

This is a full replay log, not a rendered transcript cache. Persisted events can
include prompts, tool-call inputs, tool outputs, reasoning deltas, command
metadata, context file paths, absolute workdir paths, and token usage data.

`ResidentWorkStarted` records the stable actor session, epoch, handle, and
inbox boundary, without tool output or external-effect payloads. This work
remains pending across a user-cancelled Idle state. `ResidentWorkFinished`
settles the matching epoch and advances the inbox cursor; standalone
`MailConsumed` acknowledges mail without settling the prompt. See
[user cancellation and owed work](subagent-orchestration.md#user-cancellation-and-owed-work).

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

`delete_session(session: SessionId) -> Result<bool, StoreError>` removes the
session and every descendant in the `session.parent_id` tree in one transaction:

- `event_log` (including mailbox, channel, and Workflow state),
- `open_assistant_message`, `pending_resume`, `projection_snapshot`, `file_blob`,
- `resident_actor_claim` rows for tree members,
- the materialized `session` rows themselves.

Project configuration (`project`, `project_root`), saved Project/global permissions,
and database-wide Agent/model preferences are retained. Session-scoped permission
grants are not persisted. No independent mailbox/channel tables exist.

`delete_session_at(session: SessionId, last_seq: EventSeq)` performs the same
cleanup only if the root log still ends at `last_seq`; otherwise it returns
`false` and changes nothing. Both methods return whether the root had event rows.
After commit, cached projections for the whole tree are dropped. Project queries
use the surviving session rows without compensating event-existence filters.

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
| Durable | `projection_snapshot` row per session ([schema](#projection-snapshots)) | a read that folded ≥ 1024 events since the last snapshot (every read for the first fold of an existing log above that size) | every process on the database |

A read (`read_projection`, `read_projection_shared`, `with_projection`):

1. takes the in-process projection, else the durable snapshot, as the base;
2. selects `event_log` rows of the session with `seq >= base.last_seq`, checks
   that the first row is the base's anchor (`seq == last_seq`), and applies
   only the rows after it with `Projection::apply`;
3. caches the result and, past the interval, upserts the durable snapshot.

`read_projection_shared` returns the cached `Arc<Projection>` and
`with_projection(session, |p| ...)` borrows it, so hot readers (the `wait`
tool, the steer mailbox, the `session.usage` capability, lineage walks) pay
neither a replay nor a deep clone. Read-only engine, actor scheduling, HTTP,
CLI and Workflow control paths use shared snapshots too. For example, use
`let projection = store.read_projection_shared(session).await?` to inspect a
transcript, then release it before appending when no old view is needed.
The owned `read_projection(session) -> Result<Projection, StoreError>` remains
available for callers that deliberately need a mutable copy. Transaction-local
replay returns `Arc<Projection>`; registration and revival use `Arc::make_mut`
only when they apply their own uncommitted events. Turn preparation copies its
small parent/agent metadata and releases the shared transcript after conversion,
so it does not retain an old projection through provider and tool execution. Actor wake, activation, mail delivery and archive paths likewise copy their cursor, identity and recipient values before writes or handoff generation. Descendant archive loops copy only paths and session ids before ending each member. Existing reads of post-mail inbox boundaries remain after the mail commits. Session title, summary, terminal handoff, shell and Workflow calls release their shared projection before model/tool execution; HTTP turn waits release it before each polling sleep. Handoff fallback retains only its goal, current state, pending text and carry sections, and still reads unread mail after the model call.
A read of an unchanged session costs one
indexed query that returns only the anchor row.

Cache advancement decodes the anchored tail while the old cached snapshot remains
available. It then releases the cache's matching Arc, applies that tail with
`Arc::make_mut`, and republishes synchronously under the cache mutex. No lock spans an await
and no concurrent reader can observe a missing cache entry during that transfer. A unique projection advances in place. An earlier
snapshot retained by a reader stays immutable; only that case needs a copy.
Cold replay still streams directly into the reducer. Writer transactions retain
the cache's Arc and never publish their uncommitted view.

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
running a binary with the same schema baseline but an older reducer ignores
newer snapshots the same way; the rows are rewritten by whichever reducer
reads next. The schema boundary above still rejects pre-reset databases.

**Equivalence tests.** `crates/hya-proto/tests/projection_snapshot.rs`
checks `decode(encode(fold(prefix))) + tail == fold(all)` at every split of
generated logs (streaming, usage records and legacy totals, part
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

## Write-through indexes

The event-append transaction maintains `session` placement rows from
`session_created` and the `open_assistant_message` crash-recovery index from
assistant message lifecycle events. Registration, mail, member spawn, and
reports write only to `event_log`; reads fold the shared projection. They
create no placeholder session rows or relational team/mail/task rows.

## Usage

Usage is part of the event-sourced session projection, so replay and live
reads share the same account. The engine appends `UsageRecorded` for provider
calls; finishing a message does not write a second usage record or count its
transcript text. Missing provider usage is not replaced with an estimate or
a downloaded tokenizer.

Read usage through `SessionStore::read_projection(session)` (or
`read_projection_shared` / `with_projection`), for example:

```rust
let projection = store.read_projection(session).await?;
let usage = &projection.session.usage;
let total = usage.total();
// total.prompt() = input + cache_read + cache_write; output includes thinking.
```

`SessionProjection.usage: SessionUsage` exposes `by_model:
BTreeMap<ModelRef, UsageTotals>` and `by_purpose:
BTreeMap<UsagePurpose, UsageTotals>`. Each `UsageRecorded` contains
`session: SessionId`, `message: Option<MessageId>`, `step: Option<u32>`,
`model: ModelRef`, `purpose: UsagePurpose`, and `tokens: TokenUsage`.
See [Usage attribution](runtime.md#usage-attribution) for the recording paths
and [Session usage fold](event-model.md#session-usage-fold) for the complete
counter contracts and retained legacy event fallback. There is no separate
usage table or store accounting API.

## Saved permissions

When a permission ask is answered **`always`**, the server writes one durable
row via `SavedPermissions::remember`:

| Field | Value |
| --- | --- |
| `id` | `psv_<requestId>` |
| `project_id` | **`"global"`** for exact invocation grants and action-wide `*` grants (they apply to every session); the Project id for an `ExternalDirectory` grant of one canonical directory (ADR-0026). A session-scoped grant is never persisted. |
| `action` | lowercase `Action` name (`tool`, `read`, `edit`, `glob`, `grep`, `bash`, `task`, `mcp`, `webfetch`, `websearch`, `todowrite`, `skill`, `lsp`, `externaldirectory`) |
| `resource` | remembered match pattern string |

A `"global"` row applies in every workspace for that action/pattern pair; a
Project row applies only to sessions of that Project (subject to the unique
constraint on `(project_id, action, resource)`). Deleting a Project deletes
its rows in the same transaction.

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
`native_grants` for exact `tool` / `mcp` / `bash` subjects, and
`grant_scoped(GrantScope::Project)` for Project rows), and
`DELETE /v1/permissions/rules/{rule}` revokes the in-memory grant along with
the row. See [Tools and permissions — Saved
grants](tools-and-permissions.md#saved-grants).

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

Opening the registry retries transient SQLite BUSY/LOCKED errors during connection
setup and migrations for up to five seconds, with a 10ms asynchronous backoff.
Migration history inspection and all pending migrations run under one
`BEGIN IMMEDIATE` transaction. This serializes concurrent openers across
processes before they decide which versions are missing; SQLx's SQLite
migration lock alone does not provide that serialization.
This lets simultaneous catalog refreshes open the same registry reliably. It
does not change the zero-timeout writer contract above. No configuration is
needed: `BundleRegistry::connect(path).await?` applies this policy automatically.
Non-lock errors return immediately; exhausted setup retries return the original
SQLite or migration error.

### API (`BundleRegistry`)

| Method | Result |
| --- | --- |
| `connect(path: &str)` | `Result<BundleRegistry, StoreError>`; opens/migrates with bounded setup-lock retries |
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
| `Migrate` | Migration runner failure, including unknown applied versions |
| `UnsupportedDatabase` | Existing session schema lacks the current baseline; move the database away |
| `EventDecode` | Stored event decode failed; includes session, sequence and JSON source |
| `SessionKeyData` | Stored key is not a valid `hysec_` text key |
| `Json` | Event/payload JSON (de)serialization |
| `Bundle` | `hya-bundle` prepare/catalog error |
| `BundleRegistryData` | Invalid registry generation or install candidate shape |
| `BundleRegistryCorrupt` | Stored prepared catalog bytes fail decode/validation |
| `BundleRegistryBusy` | Writer contention with zero busy timeout |
| `BundleNotFound` | Uninstall/lookup of missing bundle |
| `BundleContentConflict` | Same version, different content on install |
| `BundleAgentIdReserved` | Bundle declares a reserved built-in agent id |
| `ActorAlreadyClaimed` | Resident actor claim held by another owner |
| `StaleActorClaim` | Epoch/owner no longer current |
| `ActorClaimUnavailable` | No recoverable active claim |
| `ActorClaimData` | Claim payload/data error |
| `RuntimeOwnerBusy` | Exclusive runtime-owner lock already held |
| `RuntimeOwnerClaimRequired` | Matching runtime owner claim required for recovery |
| `RuntimeOwnerLock` | Runtime-owner lock file I/O failed |
| `WorkflowData` | Malformed or inconsistent Workflow control mutation |
| `MailboxRejected` | Mailbox write rejected |
| `ProjectNameEmpty` | Blank Project name |
| `ProjectRootsEmpty` | Project with no roots |
| `ProjectRootNotAbsolute` | Relative or empty root (or resolve path) |
| `ProjectRootInvalid` | Root (or resolve path) with a `..` component |
| `ProjectNotFound` | Rename / root replacement of a missing Project |
| `ProjectInUse` | Delete refused: non-archived root sessions still belong to the Project |
| `ProjectData` | Corrupt `project` row (unparseable id) |

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

### Runtime owner discovery

The store holds the only database advisory lock, `<canonical-db>.runtime-owner.lock`.
The kernel lock alone determines ownership. Discovery probes the existing lock
with a non-blocking shared acquisition, so probes cannot mistake one another
for an exclusive owner. It creates no lock file and trusts PID/URL metadata
only while the exclusive lock is held. A stale
PID is ignored even if an unrelated live process has reused it; stop never
signals a PID taken from an unlocked file.

On acquisition the owner truncates old PID metadata, clears stale discovery and
stop notices, then writes its decimal PID followed by a newline before opening
SQLite. Stop requests arriving after this publication survive startup. Releasing
the claim clears the PID before unlocking; a crash can leave stale content but
the kernel releases the lock. Acquisition retries for at most 200 ms to allow a
transient discovery probe to finish.
The daemon publishes `<db>.server.json` after binding its listener and removes it
before releasing ownership. `hya serve start --db sessions.db --json` reads this
metadata and checks `/v1/health`; concurrent starters attach to the same writer.
The discovery JSON contract remains `url`, `pid`, `version`, `startedAt`, with
optional `relay` and `allowHosts` fields (see the CLI Backend daemon section).

`SessionStore::connect_runtime(path: &str) -> Result<SessionStore, StoreError>`
reserves that same lock before SQLite initialization and transfers it to the
first `claim_runtime_owner(owner: OwnerRunId) -> Result<(), StoreError>` call.
Every file-backed runtime writer uses this entry point, including `exec`,
`workflow use|run|state`, `sessions archive|unarchive`, and `serve`, so concurrent
initializers cannot race on SQLite's first WAL setup. A conflicting reservation
returns `RuntimeOwnerBusy`; CLI writers route to the owner's published server
or exit 75 before touching SQLite.

`database_paths(db: &str) -> std::io::Result<Option<DatabasePaths>>` is the shared
path contract for the store and CLI. `None` denotes an empty/in-memory path or
SQLite URI. `DatabasePaths` contains `PathBuf` fields `database`, `lock`,
`discovery`, `log`, `stop`, and `relay_identity`. It resolves relative paths,
parent symlinks, and database symlinks (including dangling targets), so creation
of the target does not change the ownership identity. Filesystem errors surface
to the ownership caller.

For example, `hya exec --db sessions.db "hello"` reserves ownership before
creating the database. `hya sessions --db sessions.db` then reads it without
owning it; the same listing on a missing path reports an error and creates no
database. `connect_read_only(path: &str) -> Result<SessionStore, StoreError>`
opens SQLite in read-only mode without changing its journal mode or schema.

### Reasoning-only interrupted resident wakes

Recovery closes an unanswered wake before admitting its next directive, including
a crash after only reasoning streamed. Providers may omit reasoning-only
assistant messages, so reasoning does not establish an answer. No configuration
change is required: for example, restarting after a thinking-only interruption
produces a visible cancelled assistant answer between the old and new user
prompts. Non-empty Text and Tool parts retain their existing answer semantics.

`SessionStore::recover_resident_actor(recovered: &RecoveredActorClaim,
root: SessionId, handle: &str) -> Result<RecoveredResidentOutcome, StoreError>`
commits this closing Text message, terminal effects and work classification in
the same fenced recovery transaction. No schema or wire contract changes.

Projection snapshots use the [shared mail-record layout](event-model.md#teamprojection-mail-records).
Reducer version 8 stores each record once and keeps inbox/channel indices in the snapshot.
