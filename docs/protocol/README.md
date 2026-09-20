# hya Protocol Guide (v1)

This guide explains how to integrate any client — GUI, WebUI, CLI, service, or
a future TUI — with the hya backend over the v1 API. The same contract is served
over two transports with identical functionality:

- **HTTP/JSON + SSE + WebSocket** (documented here; see
  [`openapi.json`](./openapi.json) and the generated
  [`api-reference.md`](./api-reference.md) for the full surface).
- **gRPC** (`hya.v1` package, server reflection enabled; the proto files
  under `proto/hya/v1` are the source of truth).

## Base URL and scoping

All HTTP routes live under `/v1`. The backend serves one process-default
directory; requests that accept a scope take a `directory` field (query
parameter for GETs, body field otherwise). The `x-hya-directory` request
header overrides the field on any request. gRPC clients pass the same
values via the `hya-directory` metadata key.

## Versioning

`/v1` is additive-only within a major version: new fields and rpcs appear
without notice, unknown fields must be ignored by clients. Breaking changes
ship as `/v2` side by side.

## Serialization rules (protojson)

Bodies follow canonical protojson:

- Fields are `lowerCamelCase`; unset fields are omitted (do not treat
  absence as an error).
- Enums serialize as their full value names: `"TURN_STATE_RUNNING"`,
  `"AUTH_STATUS_CREDENTIALED"`.
- 64-bit integers serialize as strings: `"nextSeq": "42"`.
- `bytes` fields are standard base64 strings.
- Timestamps are RFC 3339 strings.

## Errors

Every failure renders:

```json
{ "error": { "code": "session_not_found", "message": "session not found: hysec_..." } }
```

Stable codes and their HTTP status / gRPC code:

| Code | HTTP | gRPC | Meaning |
| --- | --- | --- | --- |
| `invalid_argument` | 400 | `InvalidArgument` | Malformed request. |
| `not_found` | 404 | `NotFound` | Resource does not exist. |
| `session_not_found` | 404 | `NotFound` | Unknown or deleted session. |
| `permission_denied` | 403 | `PermissionDenied` | Caller not authorized. |
| `session_busy` | 409 | `FailedPrecondition` | Another run owns the session. |
| `conflict` | 409 | `FailedPrecondition` | State conflict (stale revision, patch rejection). |
| `unavailable` | 503 | `Unavailable` | Required capability not configured (e.g. no summarizer, OAuth not wired). |
| `internal` | 500 | `Internal` | Unhandled failure. |

## Pagination

Every list rpc takes `page: {cursor, limit}` and answers
`page: {nextCursor, hasMore}`. Cursors are opaque; pass `nextCursor` back
verbatim. Events use the monotonic `sinceSeq` watermark instead.

## The event-driven model

1. `POST /v1/sessions` creates a session (`agent`, `model`, `workdir`).
2. `GET /v1/bootstrap` fetches config + catalogs + pending interactions in
   one round trip at startup.
3. `POST /v1/sessions/{id}/turns` admits work — body is a `oneof` of
   `prompt`, `command`, or `shell` — and returns a `RUNNING` turn handle
   immediately. Slash commands that route to workflow features execute
   synchronously and return a `FINISHED` turn with an empty id.
4. Subscribe to `GET /v1/sessions/{id}/events/stream` (SSE) or
   `GET /v1/events/stream` (global). Every projection change arrives as a
   `StreamFrame` JSON object; terminal state arrives as `messageFinished`
   and `turnFinished`-derivable events. On lag the server sends a
   `resync` frame — resume with `ListEvents` from `lastSeq`.
5. Synchronous clients may poll `GET /v1/sessions/{id}/turns/{turn}` or use
   `POST .../turns/{turn}/wait` with `timeoutMs`. `POST .../cancel`
   requests a cooperative abort.

SSE frames are `data:` lines containing one `StreamFrame`:

```json
{ "event": { "seq": "12", "session": "hysec_...", "timeRecorded": "...", "messageFinished": { "message": "msg_...", "finish": "FINISH_REASON_STOP" } } }
```

```json
{ "resync": { "lastSeq": "40" } }
```

## Interactions (permissions and questions)

Pending permission and question requests arrive as `permissionRequested` /
`questionRequested` events (carrying an `Interaction` summary) and are
listed by `GET /v1/interactions`. Answer with
`POST /v1/interactions/{id}/respond` — body is a `oneof` of
`{permission: {allowed, persist}}` or `{question: {answer}}` /
`{question: {rejected: true}}`. The response's `applied` is `false` when
the request was already resolved (idempotent replay).

## Terminal (PTY)

`POST /v1/pty` creates a session; `POST /v1/pty/{id}/connect-token` mints a
one-time ticket. `GET /v1/pty/{id}/connect?ticket=...` upgrades to a
WebSocket speaking the same frames as the gRPC `StreamPty` rpc:

- client → server: `{"input": "<base64>"}`, `{"resize": {"cols": 120,
  "rows": 40}}`, `{"ping": true}`
- server → client: `{"output": "<base64>"}`, `{"exit": 0}`, `{"pong": true}`

The first server frame replays the current buffer. Resize currently relies
on the shell's own TTY sizing; a runtime resize API is tracked in the
consolidation plan.

## Minimal client walkthrough

```
1. GET  /v1/health                                  → verify liveness
2. GET  /v1/bootstrap                               → config + catalogs
3. POST /v1/sessions        {agent, model, workdir} → {session: {id}}
4. GET  /v1/sessions/{id}/events/stream             → SSE subscribe
5. POST /v1/sessions/{id}/turns {prompt: {text}}    → {turn: {id, state}}
6. ... consume messageStarted / partAppended / messageFinished ...
7. POST /v1/interactions/{id}/respond               → when asked
8. GET  /v1/sessions/{id}/messages                  → transcript reads
```

## Regenerating the docs

`cargo run -p xtask -- gen-api` regenerates `api-reference.md`,
`openapi.json`, and the Rust contract crate from `proto/hya/v1`. The task
fails when any rpc lacks its `// hya.http:` mapping or two rpcs claim the
same route, so documentation cannot drift from the IDL.
