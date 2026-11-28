# `hya.v2` Protocol Overview

Every client—Hya's own TUI (`packages/hya-tui`), the browser WebUI, code editors via [`hya acp`](../guide/editor-integration.md), headless CLI commands routed through the daemon, and your own custom scripts or apps—talks to the Hya server over one clean-sheet unified contract: **`hya.v2`** (15 services, 95 RPCs, defined in `proto/hya/v2/`).

Both protocols are served **on the exact same port** (`hya serve --bind 127.0.0.1:8080`):
- **HTTP/JSON + Server-Sent Events (SSE) + WebSocket** under `/v2/*` (see **[OpenAPI Schema](openapi.json)** and **[Generated API Reference](api-reference.md)**).
- **gRPC** (`hya.v2.*` over HTTP/2 `h2c`)—any request with `content-type: application/grpc*` routes automatically to `V2Grpc` on the same port and shares the exact same live server state.

---

## 1. Base rules: Host guard, scoping & JSON wire format

### Allowed `Host` names (DNS-rebinding protection)

Every HTTP request (and gRPC `:authority`) must name `localhost`, `127.0.0.1`, `[::1]`, a non-wildcard `--bind` IP, or a hostname explicitly passed via `--allow-host <name>`. Any other `Host` header is rejected immediately with `403 permission_denied`.

### Directory scoping (`directory`)

The Hya server has **no working directory of its own**. Clients pass an absolute `directory` path on the backend machine whenever calling workspace-scoped routes:
- On `GET` / `DELETE`: query parameter `?directory=/abs/path`
- On `POST` / `PUT` / `PATCH`: JSON body field `"directory": "/abs/path"`
- On session-bound calls (`CreateTurn`, `ForkSession`, `InvokeSessionBundleApi`, etc.): Hya uses the session's own recorded `workdir` and Project roots automatically.

*(Note: The removed `x-hya-directory` HTTP header returns `400 invalid_argument` if sent.)*

### Canonical ProtoJSON rules

All `/v2` JSON bodies follow standard Protobuf JSON (`protojson`):
- Field names are `lowerCamelCase`; unset/default fields are omitted.
- Enums serialize as full string names (e.g. `"TURN_STATE_RUNNING"`, `"SESSION_KIND_PROJECT"`).
- **64-bit integers (`int64`, `uint64`) serialize as strings** (e.g. `"seq": "42"`, `"input": "1250"`).
- Timestamps use RFC 3339 (`"2026-11-28T14:00:00Z"`), and `bytes` use standard base64.

---

## 2. Driving a conversation in 4 HTTP calls

### Step 1: Ensure a Project & create a Session

```sh
# 1. Find or create a Project for a folder
curl -sS -X POST http://127.0.0.1:8080/v2/projects/ensure \
  -H 'content-type: application/json' \
  -d '{"path": "/home/me/my-app"}'

# 2. Create a Session in that Project (or pass {"kind": "SESSION_KIND_TEMPORARY"} for scratch)
curl -sS -X POST http://127.0.0.1:8080/v2/sessions \
  -H 'content-type: application/json' \
  -d '{"agent": "hya-main", "model": "anthropic/claude-sonnet-4-6", "workdir": "/home/me/my-app"}'
```

*(Clients building a full UI can also call `GET /v2/bootstrap?directory=/home/me/my-app` to load providers, models, agents, skills, commands, and pending prompts in a single round trip.)*

### Step 2: Subscribe to the live SSE event stream

Open `GET /v2/sessions/{session}/events/stream` **before** submitting a turn so you don't miss early streaming deltas:

```sh
curl -N http://127.0.0.1:8080/v2/sessions/hysec_.../events/stream
```

Each SSE `data:` line is a JSON `StreamFrame`:
- **`seq == "0"` (live-only deltas):** Fast streaming text/reasoning chunks, `busy` transitions, `catalogUpdated`, `projectsUpdated`, or `serverStopping`. Not replayed from disk.
- **`seq > "0"` (durable events):** Committed messages, tool calls/results, usage records (`tokensRecorded`), todo updates, `extensionEvent` frames, and `messageFinished`.
- **`resync`:** Emitted if a slow consumer lagged behind the in-memory ring buffer—catch up via `GET /v2/sessions/{session}/events?sinceSeq=<last_seq>` or re-read `GET /v2/sessions/{session}/messages`.

### Step 3: Start a Turn (`prompt`, `command`, or `shell`)

```sh
curl -sS -X POST http://127.0.0.1:8080/v2/sessions/hysec_.../turns \
  -H 'content-type: application/json' \
  -d '{"prompt": {"text": "Run the unit tests and summarize any failures"}}'
```

`CreateTurn` returns immediately while the turn streams on SSE until `messageFinished` (`FINISH_REASON_STOP`, `FINISH_REASON_CANCELLED`, etc.). If a turn is already running on that session, a second `CreateTurn` returns `409 session_busy`—queue follow-up user messages client-side.

Simple scripts that don't want SSE can block with `POST /v2/sessions/{id}/turns/{turn}/wait?timeoutMs=60000`, or cancel a running turn with `POST /v2/sessions/{id}/turns/{turn}/cancel`.

### Step 4: Answer permission & question prompts (`Interactions`)

When a tool needs user approval (or calls `ask_user`), a `permissionRequested` or `questionRequested` frame arrives on the stream (and appears in `GET /v2/interactions`). Answer it to unblock the turn:

```sh
curl -sS -X POST http://127.0.0.1:8080/v2/interactions/perm_.../respond \
  -H 'content-type: application/json' \
  -d '{"permission": {"allowed": true, "persist": false}}'
```

Set `"persist": true` for *Always allow* (saved in `/v2/permissions/rules`).

---

## 3. Domain Extensions: `BundleApi` & `ExtensionEvent`

In `hya.v2` ([ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)), core services contain zero domain-coupled RPCs (workflows, subagent steering/archiving, channel inspection). Instead, both **Native ABI v2** (`NativeBundleApiHandler`) and **JS process** bundles expose domain routes uniformly through **`hya.v2.BundleApi`**, and domain state flows through **`Event::Extension` → `ExtensionEvent` → `SessionInfo.extensions`**:

| RPC / Route | Purpose |
| --- | --- |
| `GET /v2/bundle-apis` (`BundleApi.ListBundleApis`) | Discover every bundle endpoint visible in the requested `session`, `directory`, or global scope. |
| `ANY /v2/sessions/{session}/bundles/{bundle}/{path}` (`BundleApi.InvokeSessionBundleApi`) | Invoke a session-scoped bundle endpoint (`{bundle}` is percent-encoded, e.g. `hya%2Fworkflow` or `hya%2Fmulti-agent`). Returns the handler's HTTP status and JSON body verbatim. |
| `ANY /v2/bundles/{bundle}/api/{path}` (`BundleApi.InvokeGlobalBundleApi`) | Invoke a global (non-session) bundle endpoint (e.g. `GET /v2/bundles/hya%2Fworkflow/api/workflows?directory=/abs/path`). |

### First-party `BundleApi` routes

- **`hya/multi-agent`:** `GET .../members`, `POST .../archive`, `POST .../steer`
- **`hya/workflow`:** `GET /v2/bundles/hya%2Fworkflow/api/workflows?directory=<abs>`, `GET /v2/sessions/{id}/bundles/hya%2Fworkflow/state`, `POST /v2/sessions/{id}/bundles/hya%2Fworkflow/command`
- **`hya/agent-channels`:** `GET /v2/sessions/{id}/bundles/hya%2Fagent-channels/{channels,channels/{channel}/messages,inbox,team-status}`
- **`hya/coding-extras`:** `GET /v2/bundles/hya%2Fcoding-extras/api/lsp/symbols?directory=<abs>&query=<name>&limit=<n>` searches workspace symbols through the language servers configured in the bundle's `config.yml` (`lsp:`). `directory` is required and absolute; `limit` defaults to 10 and is capped at 100. The reply is `{"symbols": [{"path", "name", "kind", "lineNumber"}]}`: `path` relative to `directory`, `kind` one of `SYMBOL_KIND_FUNCTION`, `SYMBOL_KIND_TYPE`, `SYMBOL_KIND_MODULE`, `SYMBOL_KIND_OTHER`, `lineNumber` 1-based. No configured server answers an empty list; a server failure answers `502` with `error.code` `lsp_failed`; with the bundle disabled the route is `404` `bundle_api_not_found`. This replaces the removed `Files.SearchSymbols` (`GET /v2/fs/symbols`).

#### Workspace files and worktrees

The native `hya/workspace-files` bundle provides filesystem browsing for
frontends and Git worktree management. The full profile enables it by default.
`hya --bare` and a full profile with
`bundles.builtin.disable: [hya/workspace-files]` omit its endpoints. The TUI
uses these routes for `@file` suggestions, Project root completion, and
remote image reads. These calls replace the former `Files` and `Worktrees`
RPCs and their direct `/v2/fs/*` and `/v2/worktrees*` HTTP routes.

For HTTP, prefix the paths below with
`/v2/bundles/hya%2Fworkspace-files/api`. For gRPC, call
`BundleApi.InvokeGlobalBundleApi` with `bundle: "hya/workspace-files"`,
the indicated method and path, and the same query or body. Every call
requires an absolute `directory` string on the backend. GET and DELETE
take it in the query. POST takes it in the JSON body.

```sh
curl --get http://127.0.0.1:8080/v2/bundles/hya%2Fworkspace-files/api/fs/find \
  --data-urlencode "directory=$PWD" --data-urlencode 'pattern=**/*.rs' \
  --data-urlencode 'limit=20'
```

| Method and path | Additional input | JSON success body |
| --- | --- | --- |
| `GET /fs/read` | `path` string, `maxBytes` uint64 (0 reads the whole file) | `{content, text, mime}`. Standard base64, a UTF-8 boolean, and a MIME string. |
| `GET /fs/list` | Optional `path` string (empty lists the root) | `{entries: [{name, kind, size, timeModified}]}`, sorted by name. `kind` is `DIR_ENTRY_KIND_FILE`, `DIR_ENTRY_KIND_DIRECTORY`, or `DIR_ENTRY_KIND_SYMLINK`. `size` is a decimal byte count string and `timeModified` is RFC 3339. |
| `GET /fs/find` | `pattern` glob string, optional `limit` uint32 (500 default, 2,000 cap) | `{paths: string[]}`. Sorted relative paths. |
| `GET /fs/search` | `query` literal string, optional `glob` string and `limit` uint32 (200 default, 1,000 cap) | `{matches: [{path, lineNumber, text}]}`. Relative path, 1-based line number, and up to 400 characters of the matching line. |
| `GET /worktrees` | — | `{worktrees: Worktree[]}`. Lists linked worktrees other than the source directory. |
| `POST /worktrees` | Optional `name` string | `Worktree`. Creates a worktree from the source HEAD with a `compat/<slug>` branch. |
| `POST /worktrees/{worktree}/reset` | — | `Worktree` with `id` and `path`. Resets to the source HEAD and removes untracked and ignored files. |
| `DELETE /worktrees/{worktree}` | — | `{}`. Removes a linked worktree and its branch. Deleting a missing worktree succeeds. |

`Worktree` has string `id` and `path` fields with the absolute worktree
path and an optional string `branch`. Percent-encode the whole absolute
path as one `{worktree}` segment. Read and list accept relative paths
and absolute paths within `directory`. The bundle reports argument
errors with status 400 and an `{error: {code, message}}` body.

### `ExtensionEvent` wire shape (`proto/hya/v2/events.proto`)

Every durable `Event::Extension` emitted by a bundle streams inside `StreamEvent.extension_event` (protojson `extensionEvent`):

```json
{
  "seq": "19",
  "session": "hysec_...",
  "timeRecorded": "2026-11-28T14:00:00Z",
  "extensionEvent": {
    "sessionId": "hysec_...",
    "bundle": "hya/workflow",
    "kind": "workflow_stage_started",
    "payload": { "stage": "plan", "member": "main/hya-pir-planner-1" }
  }
}
```

Folded per-bundle state produced by each bundle's `ProjectionReducer` is returned on `SessionInfo.extensions` (a `google.protobuf.Struct` keyed by bundle ID, e.g. `session.extensions["hya/workflow"]`).

---

## 4. Session lifecycle features

- **Archived sessions:** `PATCH /v2/sessions/{id}` with `{"archived": true}` hides a finished root session from default `GET /v2/sessions` listings without deleting it (`?includeArchived=true` includes them; sending a new turn unarchives automatically).
- **Ephemeral sessions:** Passing `"ephemeral": true` on `POST /v2/sessions` marks a pre-opened empty session so the server automatically deletes it ~5 seconds after the last client stream disconnects if zero messages were ever sent.
- **Undo, Redo & Fork:** `POST /v2/sessions/{id}/revert` (`{}` to undo, `{"undo": true}` to redo) rolls back both transcript messages and modified files on disk; `POST /v2/sessions/{id}/fork` clones the session history up to the head or before a chosen `messageId`.
- **PTY WebSockets:** `POST /v2/pty` spawns a server terminal and `/v2/pty/{id}/ws` streams `PtyClientFrame` / `PtyServerFrame` JSON frames over WebSocket (or `StreamPty` over bidirectional gRPC).
- **Session-scoped MCP overlays (`OpenMcpSession`):** Local adapters such as [`hya acp`](../guide/editor-integration.md) call `POST /v2/mcp/session` (SSE) with `{workdir, model?, peers: [{name, socket}]}` to create a session backed by client-hosted MCP Unix sockets for the lifetime of the stream.
- **Failed-turn error codes:** When a turn finishes in `TURN_STATE_FAILED`, both `MessageInfo.error` and `TurnInfo.errorCode`/`errorMessage` report one of `provider_auth` (expired OAuth login or HTTP `401`/`403`), `provider_error`, `tool_error`, `store_error`, `bundle_error`, `runtime_refresh_error`, `agent_definition_missing`, `invalid`, or `turn_already_active`.
- **Server shutdown:** When the daemon stops or restarts (`hya serve stop|restart`), every open SSE and gRPC stream receives a final `serverStopping { reason: "stop" | "restart" | "signal" }` frame and closes cleanly.

---

## 5. Pagination & Error Codes

Paginated endpoints accept `?page.limit=50&page.cursor=<opaque>` and return `page: { nextCursor, hasMore }`.

Errors always return a consistent JSON envelope (`{ "error": { "code": "...", "message": "..." } }`) mapped 1:1 to HTTP and gRPC status codes:

| Error `code` | HTTP | gRPC | When it happens |
| --- | --- | --- | --- |
| `invalid_argument` | `400` | `InvalidArgument` | Bad JSON, missing `directory` scope, relative path, or invalid cursor. |
| `permission_denied` | `403` | `PermissionDenied` | Disallowed `Host` header, browser request over relay, or loopback-only `RelayControl` RPC called remotely. |
| `not_found` / `session_not_found` | `404` | `NotFound` | Unknown resource or deleted session ID. |
| `session_busy` | `409` | `FailedPrecondition` | Turn or Workflow already running on that session. |
| `failed_precondition` / `conflict` | `409` | `FailedPrecondition` | Resource busy (e.g. deleting a Project with active sessions) or stale revision. |
| `bundle_api_bad_request` / `not_found` / `method_not_allowed` / `failed` | `400`/`404`/`405`/`502` | `InvalidArgument`/`NotFound`/`Unimplemented`/`Unavailable` | Custom Bundle HTTP API (`BundleApi`) routing or handler errors. |
| `unavailable` | `503` | `Unavailable` | Server shutting down, remote relay offline, or unconfigured capability. |
| `internal` | `500` | `Internal` | Unexpected server failure. |

For every service, route, and schema field, see **[SDK & Client Integration](sdk.md)** and the **[Generated API Reference](api-reference.md)**.
