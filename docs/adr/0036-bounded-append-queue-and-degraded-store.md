# ADR-0036: Bounded Append Queue and Degraded Session Store

Date: 2026-11-20
Status: Accepted

## Context

On `origin/main`, a file-backed `SessionStore` opened an 8-connection SQLite
pool in WAL mode with a 5 s `busy_timeout`, mixed read and write transactions on
that shared pool, and used deferred `pool.begin()` transactions alongside
`BEGIN IMMEDIATE` on multi-event batches. Under heavy disk stall or contention,
slow writes, deferred read-to-write upgrades (`SQLITE_BUSY_SNAPSHOT`), or
pool exhaustion surfaced transient SQLite busy/locked errors directly to
callers, aborting in-flight `hya` runs. Opening a slow or unwritable database
path similarly stalled up to pool acquisition timeouts or aborted startup
instead of continuing without disk persistence.

Session persistence guarantees require that storage stalls, locks, full disks, or
unopenable/replaced paths do not terminate a run that can continue, that waiting
on storage is strictly bounded, that the in-process event log remains complete
and ordered, and that exit surfaces any durability gap or fallback explicitly
without changing the database engine or regressing healthy disk performance.

## Decision

- **One SQLite database.** Every file-backed store keeps one SQLite WAL
  database (`synchronous = NORMAL`) at the requested path. There is no
  alternate event-log backend, no silent fallback to another journal mode (for
  example `DELETE`), and no automatic relocation to another filesystem path.
- **Split pinned-writer and read-only pools.** `SessionStore` separates:
  - a single-connection writer pool (`max_connections(1)`,
    `test_before_acquire(false)`, `max_lifetime(None)`, `idle_timeout(None)`)
    whose `after_release` hook checks transaction depth and rolls back/discards
    poisoned connections without closing a healthy writer mid-run; and
  - a lazy read-only pool (`max_connections(4)`, `read_only(true)`).
- **Single writer task with an ordered append queue and in-process sequence
  numbers.** All append paths (`append_event`, `append_events`,
  `append_events_with_blobs`, `put_file_blob`, and `commit_tool_result_with_mail`
  without a resident claim) assign monotonic `EventSeq` values in process
  (seeded from `MAX(seq)` / `sqlite_sequence` on open and raised on every
  `StoreTransaction` guard release) and submit operations in order to one
  writer task executing `BEGIN IMMEDIATE` transactions. Healthy appends wait up
  to `WRITE_TIMEOUT` (2 s); on timeout or a SQLite storage fault (`PoolTimedOut`,
  `Io`, `WorkerCrashed`, or primary error code `3 | 5 | 6 | 8 | 10 | 11 | 13 |
  14 | 15 | 26` modulo 256), the store transitions to `StoreHealth::Degraded`,
  updates or initializes the in-memory `ProjectionCache`, returns `Ok` to the
  caller, and retries queued operations in submission order with exponential
  backoff (`25 ms` .. `250 ms`). Logical errors during queued execution
  propagate without degrading (when synchronous) or halt flushing (when already
  acknowledged), leaving a harmless gap in `seq` rather than rolling back an
  in-memory counter.
- **Raw transactions stay fenced.** `SessionStore::begin_transaction` waits behind
  queued appends ahead of it up to `WRITE_TIMEOUT` when healthy, then waits
  unboundedly on SQLite `BEGIN IMMEDIATE` (matching `origin/main`), refuses
  immediately with `StoreError::StoreDegraded` when degraded or halted, and
  resolves `StoreTransaction::commit` in an owned spawned task so caller
  cancellation cannot release the guard while SQLite is still committing.
- **Database file identity and primary-key conflict protection.** At
  `WriteQueue::start`, the store records the database file's `(dev, ino)`
  (Unix `stat` on a detached OS thread). Before each writer operation, raw
  transaction, disk read, and clean exit report, it checks the path again via a
  blocking `stat` answered from the kernel inode cache (~0.7 µs on local
  filesystems); if the file is missing (`NotFound`/`NotADirectory`), replaced
  (device/inode changed), or truncated to 0 bytes (partial non-zero truncation
  is not checked by `stat`), the writer halts permanently for the remainder of
  the process, disk reads refuse with `StoreError::StoreDegraded`, and cached
  or queued reads stay in memory without touching a replacement file (transient
  metadata errors such as `PermissionDenied` or `EIO` degrade and retry instead
  of halting). If an `INSERT` fails with an `event_log.seq` primary-key conflict
  (extended code `1555` or primary `19` naming `event_log.seq`), the writer
  queries `event_log` for the queued events: identical rows (`session_id`,
  `payload`, `ts`) mean an earlier cancelled/timed-out attempt landed and the
  batch is treated as committed; any difference indicates a foreign writer and
  permanently halts flushing. The writer never renumbers sequences or writes
  around a conflict.
- **Open policy (`OPEN_TIMEOUT` = 5 s).** Path resolution, runtime-owner locking,
  and initial file `stat` run on detached OS threads inside `OPEN_TIMEOUT` (5 s)
  so a hung filesystem during open never stalls runtime shutdown (per-operation
  `stat` checks on the writer task, disk reads, and exit drain remain blocking
  calls answered from the kernel inode cache). Servers (`serve`, detached
  daemon), embedded `HyaRuntime`, and existing-session commands
  (`sessions archive|unarchive`, `workflow use|state`, `workflow run --session`)
  use strict `SessionStore::connect_runtime` and propagate open failures or
  timeouts. One-shot headless runs (`exec`, `exec --bare`, `-p`, direct
  `workflow run`) use `SessionStore::connect_runtime_or_memory`, falling back to
  an in-memory store for their entire lifetime with one warning line on stderr
  when opening exceeds `OPEN_TIMEOUT` (5 s) or fails with a storage fault.
  `StoreError::RuntimeOwnerBusy` (exit 75), `StoreError::UnsupportedDatabase`,
  and invalid input paths (`InvalidInput`, such as database URIs or symlink
  loops) never fall back to memory.
- **Exit drain and stderr report (`EXIT_DRAIN_TIMEOUT` = 30 s).** Every
  store-owning exit path (bare `exec --bare` and `--json`, full-mode headless
  runs via `BuiltSessionEngine::shutdown` and `startup::start`, `hya serve` /
  `HyaRuntime` post-open startup errors and shutdown, and direct
  `hya workflow` / `hya sessions archive|unarchive`, including `SIGINT` and
  `SIGTERM`) drains pending appends up to `EXIT_DRAIN_TIMEOUT` (30 s; returning
  immediately if the writer is halted or running in an open-time fallback) and
  emits at most one stderr status line via `drain_and_report_exit` when the run
  degraded, left unpersisted events/blobs (exact event count and byte total
  including payloads and blobs, path, and reason), or ran in memory — leaving
  `--json` stdout strictly pure JSONL (uncatchable `SIGKILL` cannot run drain).
- **Read fallbacks only at named reads (`READ_TIMEOUT` = 5 s).** Bounded read
  fallbacks apply only at the four named callers: `cached_projection` anchored
  follow-up fold, `hya-core` lazy file-blob budget check (`BlobBudget::keep_prior`,
  warning via `tracing::warn!`), `SessionEngine` in-turn effort preference reads
  after a first successful read for that agent or model (warning via
  `tracing::warn!`), and local full-mode `exec --json` lag/final replay. All
  other healthy reads remain unbounded as on `origin/main`.
- **Constants only.** `OPEN_TIMEOUT`, `WRITE_TIMEOUT`, `READ_TIMEOUT`, and
  `EXIT_DRAIN_TIMEOUT` are fixed compile-time constants with no CLI flags or
  user configuration knobs; test harnesses inject shorter bounds through
  `#[doc(hidden)]` constructors.

## Consequences

- **Degraded crash durability:** While the store is degraded, halted, or in an
  open-time in-memory fallback, acknowledged events and blobs live only in
  process memory until flushed or reported at exit; an external `SIGKILL` during
  such an outage loses unpersisted queued events.
- **No multi-writer sharing or silent journal-mode downgrade:** Filesystems
  without working WAL locks/shared memory or conflicting writers cannot trigger
  rollback-journal writes or silent renumbering; unopenable or lockless opens
  fall back explicitly to memory and report zero persistence.
- **Unbounded queue memory within a single outage:** Unflushed events and
  `file_blob` payloads generated during a prolonged disk outage remain buffered
  in the process queue until storage recovers or the run exits.

See [Event Model & Storage](../architecture/events-and-storage.md) and [CLI Reference](../reference/cli.md).
