# `hya.v1` Protocol Overview

Every client—Hya's own TUI, the browser WebUI, code editors via [`hya acp`](../guide/editor-integration.md), headless CLI commands routed through the daemon, and your own custom scripts or apps—talks to the Hya server over one unified contract: **`hya.v1`** (18 services, 109 RPCs, defined in `proto/hya/v1/`).

Both protocols are served **on the exact same port** (`hya serve --bind 127.0.0.1:8080`):
- **HTTP/JSON + Server-Sent Events (SSE) + WebSocket** under `/v1/*` (see **[OpenAPI Schema](openapi.json)** and **[Generated API Reference](api-reference.md)**).
- **gRPC** (`hya.v1.*` over HTTP/2 `h2c`)—any request with `content-type: application/grpc*` routes automatically to the gRPC services on the same port and shares the exact same live server state.

---

## 1. Base rules: Host guard, scoping & JSON wire format

### Allowed `Host` names (DNS-rebinding protection)

Every HTTP request (and gRPC `:authority`) must name `localhost`, `127.0.0.1`, `[::1]`, a non-wildcard `--bind` IP, or a hostname explicitly passed via `--allow-host <name>`. Any other `Host` header is rejected immediately with `403 permission_denied`.

### Directory scoping (`directory`)

The Hya server has **no working directory of its own**. Clients pass an absolute `directory` path on the backend machine whenever calling workspace-scoped routes:
- On `GET` / `DELETE`: query parameter `?directory=/abs/path`
- On `POST` / `PUT` / `PATCH`: JSON body field `"directory": "/abs/path"`
- On session-bound calls (`CreateTurn`, `ForkSession`, `GetWorkflowState`, etc.): Hya uses the session's own recorded `workdir` and Project roots automatically.

*(Note: The legacy `x-hya-directory` HTTP header is retired and returns `400 invalid_argument` if sent.)*

### Canonical ProtoJSON rules

All `/v1` JSON bodies follow standard Protobuf JSON (`protojson`):
- Field names are `lowerCamelCase`; unset/default fields are omitted.
- Enums serialize as full string names (e.g. `"TURN_STATE_RUNNING"`, `"SESSION_KIND_PROJECT"`).
- **64-bit integers (`int64`, `uint64`) serialize as strings** (e.g. `"seq": "42"`, `"input": "1250"`).
- Timestamps use RFC 3339 (`"2026-11-26T14:00:00Z"`), and `bytes` use standard base64.

---

## 2. Driving a conversation in 4 HTTP calls

### Step 1: Ensure a Project & create a Session

```sh
# 1. Find or create a Project for a folder
curl -sS -X POST http://127.0.0.1:8080/v1/projects/ensure \
  -H 'content-type: application/json' \
  -d '{"path": "/home/me/my-app"}'

# 2. Create a Session in that Project (or pass {"kind": "SESSION_KIND_TEMPORARY"} for scratch)
curl -sS -X POST http://127.0.0.1:8080/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"agent": "hya-main", "model": "anthropic/claude-sonnet-4-6", "workdir": "/home/me/my-app"}'
```

*(Clients building a full UI can also call `GET /v1/bootstrap?directory=/home/me/my-app` to load providers, models, agents, skills, commands, and pending prompts in a single round trip.)*

### Step 2: Subscribe to the live SSE event stream

Open `GET /v1/sessions/{session}/events/stream` **before** submitting a turn so you don't miss early streaming deltas:

```sh
curl -N http://127.0.0.1:8080/v1/sessions/hysec_.../events/stream
```

Each SSE `data:` line is a JSON `StreamFrame`:
- **`seq == "0"` (live-only deltas):** Fast streaming text/reasoning chunks, `busy` transitions, `catalogUpdated`, or `serverStopping`. Not replayed from disk.
- **`seq > "0"` (durable events):** Committed messages, tool calls/results, usage records (`tokensRecorded`), todo updates, and `messageFinished`.
- **`resync`:** Emitted if a slow consumer lagged behind the in-memory ring buffer—catch up via `GET /v1/sessions/{session}/events?sinceSeq=<last_seq>` or re-read `GET /v1/sessions/{session}/messages`.

### Step 3: Start a Turn (`prompt`, `command`, or `shell`)

```sh
curl -sS -X POST http://127.0.0.1:8080/v1/sessions/hysec_.../turns \
  -H 'content-type: application/json' \
  -d '{"prompt": {"text": "Run the unit tests and summarize any failures"}}'
```

`CreateTurn` returns immediately while the turn streams on SSE until `messageFinished` (`FINISH_REASON_STOP`, `FINISH_REASON_CANCELLED`, etc.). If a turn is already running on that session, a second `CreateTurn` returns `409 session_busy`—queue follow-up user messages client-side.

Simple scripts that don't want SSE can block with `POST /v1/sessions/{id}/turns/{turn}/wait?timeoutMs=60000`, or cancel a running turn with `POST /v1/sessions/{id}/turns/{turn}/cancel`.

### Step 4: Answer permission & question prompts (`Interactions`)

When a tool needs user approval (or calls `ask_user`), an `interactionRequested` frame arrives on the stream (and appears in `GET /v1/interactions`). Answer it to unblock the turn:

```sh
curl -sS -X POST http://127.0.0.1:8080/v1/interactions/perm_.../respond \
  -H 'content-type: application/json' \
  -d '{"permission": {"allowed": true, "persist": false}}'
```

Set `"persist": true` for *Always allow* (saved in `/v1/permissions/rules`).

---

## 3. Session lifecycle features

- **Archived sessions:** `PATCH /v1/sessions/{id}` with `{"archived": true}` hides a finished root session from default `GET /v1/sessions` listings without deleting it (`?includeArchived=true` includes them; sending a new turn unarchives automatically).
- **Ephemeral sessions:** Passing `"ephemeral": true` on `POST /v1/sessions` marks a pre-opened empty session so the server automatically deletes it ~5 seconds after the last client stream disconnects if zero messages were ever sent.
- **Undo, Redo & Fork:** `POST /v1/sessions/{id}/revert` (`{}` to undo, `{"undo": true}` to redo) rolls back both transcript messages and modified files on disk; `POST /v1/sessions/{id}/fork` clones the session history up to the head or before a chosen `messageId`.
- **PTY WebSockets:** `POST /v1/pty` spawns a server terminal and `/v1/pty/{id}/ws` streams `PtyClientFrame` / `PtyServerFrame` JSON frames over WebSocket (or `StreamPty` over bidirectional gRPC).
- **Session-scoped MCP overlays (`OpenMcpSession`):** Local adapters such as [`hya acp`](../guide/editor-integration.md) call `POST /v1/mcp/session` (SSE) with `{workdir, model?, peers: [{name, socket}]}` to create a session backed by client-hosted MCP Unix sockets for the lifetime of the stream.
- **Failed-turn error codes:** When a turn finishes in `TURN_STATE_FAILED`, both `MessageInfo.error` and `TurnInfo.errorCode`/`errorMessage` report one of `provider_auth` (expired OAuth login or HTTP `401`/`403`), `provider_error`, `tool_error`, `store_error`, `bundle_error`, `runtime_refresh_error`, `agent_definition_missing`, `invalid`, or `turn_already_active`.
- **Server shutdown:** When the daemon stops or restarts (`hya serve stop|restart`), every open SSE and gRPC stream receives a final `serverStopping { reason: "stop" | "restart" | "signal" }` frame and closes cleanly.

---

## 4. Pagination & Error Codes

Paginated endpoints accept `?page.limit=50&page.cursor=<opaque>` and return `page: { nextCursor, hasMore }`.

Errors always return a consistent JSON envelope (`{ "error": { "code": "...", "message": "..." } }`) mapped 1:1 to HTTP and gRPC status codes:

| Error `code` | HTTP | gRPC | When it happens |
| --- | --- | --- | --- |
| `invalid_argument` | `400` | `InvalidArgument` | Bad JSON, missing `directory` scope, relative path, or invalid cursor. |
| `permission_denied` | `403` | `PermissionDenied` | Disallowed `Host` header, browser request over relay, or loopback-only `RelayControl` RPC called remotely. |
| `not_found` / `session_not_found` | `404` | `NotFound` | Unknown resource or deleted session ID. |
| `session_busy` | `409` | `FailedPrecondition` | Turn or Workflow already running on that session. |
| `failed_precondition` / `conflict` | `409` | `FailedPrecondition` | Resource busy (e.g. deleting a Project with active sessions) or stale revision. |
| `bundle_api_bad_request` / `not_found` / `method_not_allowed` / `failed` | `400`/`404`/`405`/`502` | `InvalidArgument`/`NotFound`/`Unimplemented`/`Unavailable` | Custom Bundle HTTP API (`apis:`) routing or process errors. |
| `unavailable` | `503` | `Unavailable` | Server shutting down, remote relay offline, or unconfigured capability. |
| `internal` | `500` | `Internal` | Unexpected server failure. |

For every service, route, and schema field, see **[SDK & Client Integration](sdk.md)** and the **[Generated API Reference](api-reference.md)**.
