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

## Migrations

The first migration is
[`0001_init.sql`](../../crates/hya-store/migrations/0001_init.sql). It creates
tables for:

- sessions, messages, and parts
- event log
- team runs and members
- mail and task board state
- goals
- token ledger

The current runtime read path is event-log based. Tables such as `message` and
`part` exist in the schema, but `read_projection` currently folds from
`event_log` rather than querying materialized message rows.

## Event Log

`append_event` inserts:

- session storage key bytes (`hysec_...` ASCII bytes for new sessions; 16-byte
  UUID keys for legacy sessions)
- serialized `Event` JSON
- timestamp in Unix epoch milliseconds

SQLite assigns the monotonic `seq`, which becomes `Envelope.seq`.

This is a full replay log, not a rendered transcript cache. Persisted events can
include prompts, tool-call inputs, tool outputs, reasoning deltas, command
metadata, context file paths, absolute workdir paths, and token usage data.

`replay(session)` loads all rows for one session ordered by `seq` and deserializes
each payload into an `Envelope`.

## Projection Reads

`read_projection(session)` is intentionally simple:

```text
replay(session) -> Projection::from_events(envelopes)
```

This keeps store replay, HTTP event reads, SSE recovery, transcript rendering,
and TUI state on the same reducer semantics.

## Token Ledger

`record_usage` writes token accounting rows into `token_ledger`.
`read_usage` returns rows for a session ordered by timestamp.

The ledger records:

- session
- role
- iteration
- completion run id
- prompt tokens
- completion tokens
- confidence label

Provider usage reporting is represented in the data model, but live HTTP routes
currently declare `usage_reporting: false`.

## Replay Surfaces

The same store replay powers:

- `SessionEngine::replay`
- `GET /sessions/:id/events`
- `hya-backend tail-session`
- `read_projection`

This makes the database a useful debugging artifact: if the event log is intact,
the session can be reconstructed.
