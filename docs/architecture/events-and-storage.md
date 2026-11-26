# Event Model & SQLite Storage

`crates/hya-proto` defines Hya's wire/domain event vocabulary, and `crates/hya-store` persists it in SQLite WAL mode. Together they guarantee that crashes, restarts, and concurrent clients never corrupt or fork session state.

---

## 1. Envelopes, Sequence Numbers & `Projection` (`hya-proto`)

Every event travels inside an `Envelope`:

```rust,ignore
pub struct Envelope {
    pub seq: EventSeq,      // 0 = live transient delta; > 0 = durable SQLite row
    pub ts_millis: u64,
    pub event: Event,
}
```

### Live (`seq = 0`) vs Durable (`seq > 0`)

- **`seq = 0`:** Emitted on the in-memory `EventBus` for token-by-token `TextDelta` / `ReasoningDelta` streaming, `sessionUpdated { busy }`, `projectsUpdated`, `catalogUpdated`, and `serverStopping`. Never written to SQLite and never replayed.
- **`seq > 0`:** Assigned strictly monotonically inside SQLite write transactions. Every `MessageStarted` for an assistant turn is guaranteed a matching terminal `MessageFinished` (even across `SIGINT`, daemon shutdown, or next-boot crash recovery).

### Deterministic `Projection` Reducer

`hya_proto::Projection` folds an ordered stream of durable `Envelope`s into read-ready session state:
- Messages, parts (`Text`, `Reasoning`, `Tool` with status/output/eviction notice), per-round & total `TokenUsage`, active `todo` items, `revert` overlay state, Workflow state, and Team `Roster` + `Channel` cursors.
- `Projection::apply` is idempotent on `seq <= last_seq`, allowing `hya-store` to cache serialized `projection_snapshot` checkpoints in SQLite and replay only the tail of new events on read.

---

## 2. SQLite Schema Overview (`hya-store`)

`hya-store` runs SQLite in `WAL` journal mode with foreign keys enabled:

| Table | What it stores |
| --- | --- |
| `session` | Root and child session metadata (`id`, `parent_id`, `project_id`, `kind`, `created_at`, `archived_at`, `ephemeral`). |
| `event_log` | Append-only `(seq INTEGER PRIMARY KEY, session_id, ts_millis, payload JSON)` truth. |
| `projection_snapshot` | Versioned cached `Projection` blobs + `last_seq` for fast cold reads on long sessions. |
| `project` / `project_root` | Client-managed Projects (`prj_...`) and their ordered canonical root paths ([ADR-0024](../adr/0024-project-model-and-client-chosen-workspace.md)). |
| `resident_actor_claim` | Active `(session_id, actor_epoch, owner_run_id)` leases fencing subagent episodes against zombie writes. |
| `pending_resume` | Root sessions interrupted by a graceful daemon `stop`/`restart` awaiting a restore prompt on next start. |
| `saved_permission` | User *"Always allow"* grants (global for tools/commands, Project-scoped for `ExternalDirectory`). |
| `file_blob` | Content-addressed file snapshots captured before `write`/`edit`/`apply_patch`/`bash` to power `/undo` & `/redo`. |

*(User-installed `.hyabundle` packages live in a separate SQLite database at `~/.local/share/hya/bundles/registry.sqlite3` so wiping session history never deletes installed bundles.)*

---

## 3. Single-Writer Lock & Bounded Degraded Queue

### Kernel `flock` ownership (`<db>.runtime-owner.lock`)

Before opening SQLite in read-write mode, a server or headless writer acquires an exclusive advisory kernel lock on `<resolved-db>.runtime-owner.lock` ([ADR-0022](../adr/0022-one-writer-per-database.md)):
- Plain paths are canonicalized (resolving symlinks); `sqlite:` and `file:` URI spellings are rejected up front.
- If a daemon already holds the lock and published `<db>.server.json`, CLI writers (`exec`, `workflow`, `sessions archive`) route over `/v1` to that daemon; unroutable flags exit **`75`**.
- Read-only commands (`hya sessions`, `hya tail-session`) open SQLite read-only without touching the owner lock.

### Bounded append queue & graceful exit drain ([ADR-0036](../adr/0036-bounded-append-queue-and-degraded-store.md))

Disk hiccups (temporary `ENOSPC`, slow network mounts, or brief SQLite lock contention) should not panic an active turn:
- One-shot headless commands (`exec`, `-p`, direct `workflow run`) fall back to an in-memory store with a clear `stderr` warning if opening the DB fails or exceeds `OPEN_TIMEOUT` (5s); servers and existing-session commands fail closed instead.
- At shutdown, every store owner runs a bounded exit drain (`EXIT_DRAIN_TIMEOUT` = 30s) to flush queued event batches and prints an explicit `stderr` durability report if any events remained unpersisted.
