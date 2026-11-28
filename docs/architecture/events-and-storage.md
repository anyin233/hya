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

### Deterministic `Projection` Reducer & `Event::Extension`

`hya_proto::Projection` folds an ordered stream of durable `Envelope`s into read-ready session state:
- Core kernel events fold messages, parts (`Text`, `Reasoning`, `Tool` with status/output/eviction notice), per-round & total `TokenUsage`, active `todo` items ($g$), and `revert` overlay state.
- Bundle-owned domain events arrive as `Event::Extension { session, bundle, kind, payload }` and are folded deterministically by each bundle's registered ABI v2 `ProjectionReducer` into `Projection::extensions: BTreeMap<String, Value>` (keyed by bundle ID, such as `"hya/multi-agent"`, `"hya/agent-channels"`, `"hya/workflow"`, `"hya/compaction"`).
- `Projection::apply` is idempotent on `seq <= last_seq`, allowing `hya-store` to cache serialized `projection_snapshot` checkpoints in SQLite and replay only the tail of new events on read.

---

## 2. SQLite v2 Schema Overview (`hya-store`, `format_version = 2`)

`hya-store` runs SQLite in `WAL` journal mode with foreign keys enabled and enforces `schema_meta.format_version = 2` ([ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)). Opening an unmigrated v1 database fails fast and directs you to run `hya-migration run` (see **[Migrating from Hya v1 to v2](../guide/migration-v1-to-v2.md)**).

| Table | What it stores |
| --- | --- |
| `schema_meta` | Schema metadata including `format_version = '2'`. |
| `session` | Root and child session metadata (`id`, `parent_id`, `project_id`, `kind`, `created_at`, `archived_at`, `ephemeral`). |
| `event_log` | Append-only `(seq INTEGER PRIMARY KEY, session_id, ts_millis, payload JSON)` truth (kernel events + `Event::Extension`). |
| `projection_snapshot` | Versioned cached `Projection` blobs + `last_seq` for fast cold reads on long sessions. |
| `project` / `project_root` | Client-managed Projects (`prj_...`) and their ordered canonical root paths ([ADR-0024](../adr/0024-project-model-and-client-chosen-workspace.md)). |
| `bundle_state` | Generic `(bundle, scope, key)` JSON key-value store with optimistic-concurrency `version` CAS, exposed to native bundles via `HostBundleStorage`. |
| `bundle_claim` | Generic `(bundle, resource_key)` fenced leases (`owner_id`, monotonic `epoch`, `expires_at_millis`, `metadata`) replacing v1's domain-specific `resident_actor_claim` table. |
| `pending_resume` | Root sessions interrupted by a graceful daemon `stop`/`restart` awaiting a restore prompt on next start. |
| `saved_permission` | User *"Always allow"* grants (global for tools/commands, Project-scoped for `ExternalDirectory`). |
| `file_blob` | Content-addressed file snapshots captured before `write`/`edit`/`apply_patch`/`bash` to power `/undo` & `/redo`. |

*(User-installed `.hyabundle` packages live in a separate SQLite database at `~/.local/share/hya/bundles/registry.sqlite3` so wiping session history never deletes installed bundles.)*

---

## 3. Single-Writer Lock & Bounded Degraded Queue

### Kernel `flock` ownership (`<db>.runtime-owner.lock`)

Before opening SQLite in read-write mode, a server or headless writer acquires an exclusive advisory kernel lock on `<resolved-db>.runtime-owner.lock` ([ADR-0022](../adr/0022-one-writer-per-database.md)):
- Plain paths are canonicalized (resolving symlinks); `sqlite:` and `file:` URI spellings are rejected up front.
- If a daemon already holds the lock and published `<db>.server.json`, CLI writers (`exec`, `workflow`, `sessions archive`) route over `/v2` to that daemon; unroutable flags (`--bare`, `--pure`) exit **`75`**.
- Read-only commands (`hya sessions`, `hya tail-session`) open SQLite read-only without touching the owner lock.

### Bounded append queue & graceful exit drain ([ADR-0036](../adr/0036-bounded-append-queue-and-degraded-store.md))

Disk hiccups (temporary `ENOSPC`, slow network mounts, or brief SQLite lock contention) should not panic an active turn:
- One-shot headless commands (`exec`, `-p`, direct `workflow run`) fall back to an in-memory store with a clear `stderr` warning if opening the DB fails or exceeds `OPEN_TIMEOUT` (5s); servers and existing-session commands fail closed instead.
- At shutdown, every store owner runs a bounded exit drain (`EXIT_DRAIN_TIMEOUT` = 30s) to flush queued event batches and prints an explicit `stderr` durability report if any events remained unpersisted.
