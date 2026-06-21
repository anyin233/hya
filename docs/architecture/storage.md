# Storage

The storage layer lives in [`../../crates/yaca-store`](../../crates/yaca-store)
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
connection. The CLI uses in-memory stores for the TUI, `exec`, and goal mode.

## Migrations

The first migration is
[`0001_init.sql`](../../crates/yaca-store/migrations/0001_init.sql). It creates
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

- binary session id
- serialized `Event` JSON
- timestamp in Unix epoch milliseconds

SQLite assigns the monotonic `seq`, which becomes `Envelope.seq`.

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
- `yaca tail-session`
- `read_projection`

This makes the database a useful debugging artifact: if the event log is intact,
the session can be reconstructed.
