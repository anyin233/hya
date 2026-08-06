# Server and Client

The server lives in [`../../crates/hya-server`](../../crates/hya-server). It
wraps `SessionEngine` with Axum routes, native SSE streams, and
Compat-compatible HTTP/SSE route groups.

## App State

`AppState` contains:

- shared `SessionEngine`
- process-level `AgentSpec`
- pending permission/question queues
- a dependency-inverted MCP control handle supplied by `hya-app`
- workspace adapter metadata
- formatter status

The router wraps it into internal `ServerState`, which adds run tokens for
busy/abort behavior plus process-local global, project, PTY, and TUI state used
by compatibility routes. MCP routes do not own a manager or status map: the
control handle mutates app-owned desired state and composes status from
desired/observed state plus the effective registry manifest. The native routes run prompts through
the server's configured `AgentSpec`. Compat-compatible routes translate
Compat-shaped request/response bodies to the same engine, event log,
projection, run registry, and pending queues.

## Native Routes

| Method | Path | Request | Response |
| --- | --- | --- | --- |
| `POST` | `/sessions` | `CreateSessionRequest` | `CreateSessionResponse` |
| `POST` | `/sessions/:id/prompt` | `PromptRequest` | `PromptResponse` |
| `POST` | `/sessions/:id/command` | `CommandRequest` | `PromptResponse` |
| `POST` | `/sessions/:id/shell` | `ShellRequest` | `PromptResponse` |
| `GET` | `/sessions/:id/events` | optional `since_seq` query | `Vec<Envelope>` |
| `GET` | `/sessions/:id/stream` | none | SSE stream of envelopes |

Session ids in native URL paths accept any valid shared `SessionId` form:
`hysec_...`, `ses_...`, or legacy raw UUID.

### Status codes (`ApiError`)

Native and Compat handlers share `ApiError` constructors
([`lib.rs`](../../crates/hya-server/src/lib.rs)):

| Status | Constructor | Typical use |
| --- | --- | --- |
| `400 Bad Request` | `bad_request` | Unparseable session id (`invalid session id`); invalid Compat request bodies. |
| `404 Not Found` | `not_found` | Missing resources on Compat routes (for example message/part not found). Native turn paths do **not** map engine “session not found” here — see below. |
| `409 Conflict` | `conflict` | Busy session (`session busy`) when a second run is started while one is active. |
| `503 Service Unavailable` | `service_unavailable` | Compat paths such as MCP control-handle failures and unavailable compact/summarize operations. |
| `500 Internal Server Error` | `internal` | Every `CoreError` and `StoreError` conversion (`From` impls), including engine `Invalid("session not found")` on native admit/turn when the session is missing. |

Compatibility routes may also emit typed Compat error bodies (for example
`ProviderNotFoundError`, `PermissionNotFoundError`) with their own status codes
instead of plain `ApiError` text.

## Native Session Calls

`POST /sessions` accepts:

```json
{
  "agent": "build",
  "model": "claude-sonnet-4-6",
  "workdir": ".",
  "parent": null
}
```

and returns:

```json
{
  "session": "..."
}
```

### Prompt, command, and shell bodies

DTOs are defined in
[`hya-proto` `api.rs`](../../crates/hya-proto/src/api.rs).

**`PromptRequest`** — `POST /sessions/:id/prompt`:

```json
{
  "text": "summarize this repo"
}
```

Admits a user prompt, runs one assistant turn, and returns:

**`PromptResponse`** (also returned by `command` and `shell`):

```json
{
  "message": "<user MessageId>",
  "finish": "stop"
}
```

`finish` is a `FinishReason` enum value (for example `stop`, `tool_calls`,
`length`, `error`, depending on the turn outcome).

**`CommandRequest`** — `POST /sessions/:id/command`:

```json
{
  "command": "init",
  "arguments": "",
  "text": null
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `command` | yes | Slash command name (without leading `/`). |
| `arguments` | yes | Argument string (may be empty). |
| `text` | no | Optional full user message to admit. |

When `text` is absent, the server synthesizes the admitted user message as
`/<command>` if `arguments` is empty/whitespace, otherwise
`/<command> <arguments>`. The engine still records command metadata
(`admit_command_prompt`) before running the turn.

**`ShellRequest`** — `POST /sessions/:id/shell`:

```json
{
  "command": "ls -la"
}
```

Runs the shell tool directly and records a synthetic assistant tool-result
message, returning the same `PromptResponse` shape.

## Native Events

`GET /sessions/:id/events` replays stored envelopes for a session. Use
`?since_seq=<n>` to receive only envelopes whose sequence is greater than `n`.

`GET /sessions/:id/stream` subscribes to the engine event bus and emits SSE
events for the requested session. If the broadcast receiver lags, the server
emits an SSE event named `resync`; clients should use the events endpoint with
their last seen sequence to catch up.

## Compat-Compatible Route Groups

`compat::router()` is merged into the same Axum app. Current route groups
include:

| Group | Examples | Backing implementation |
| --- | --- | --- |
| Sessions | `/session`, `/session/:id`, `/api/session`, `/api/session/:id/context`, `/api/session/:id/message`, prompt/command/shell/abort/fork/share/update/delete/revert/summarize routes | hya event log, projection, run registry, switch/session-state events, pending queues |
| Events | `/event`, `/api/event`, `/global/event` | translated live hya envelopes plus Compat heartbeat/connected/status/error frames |
| Files/search | `/file`, `/file/content`, `/find`, `/find/file`, `/find/symbol`, `/api/fs/read/*path`, `/api/fs/list`, `/api/fs/find` | filesystem reads, ignore matching, MIME sniffing, fuzzy path search, optional `LspPlane` |
| Catalogs/metadata | `/path`, `/agent`, `/command`, `/skill`, `/lsp`, `/formatter`, `/api/location`, `/api/agent`, `/api/command`, `/api/skill` | built-in catalog sources, prompt directories, local skills, formatter/LSP planes |
| Provider/auth | `/config`, `/config/providers`, `/provider`, `/provider/auth`, `/auth/:providerID`, `/api/provider`, `/api/model`, credential/integration routes | resolved hya provider catalog and local auth token store; runtime config bag (not `config.yaml`) |
| Permissions/questions | `/permission`, `/question`, `/api/permission/*`, `/api/question/*`, session-scoped pending queues | hya ask/question channels and SQLite-backed saved permissions |
| MCP | `/mcp`, `/mcp/:name/connect`, `/mcp/:name/disconnect`, auth routes | narrow app-supplied reconciliation control handle; one runtime effective registry |
| PTY | `/pty/*`, `/api/pty/*` | in-process PTY metadata and websocket shell attach lifecycle |
| VCS/project/worktree | `/vcs/*`, `/project/*`, `/experimental/project/*/copy`, `/experimental/worktree/*` | git commands, project state, git worktree helpers |
| TUI/global/sync/experimental | `/tui/*`, `/global/*`, `/sync/*`, `/experimental/*` | process-local compatibility queues/state and event-log-backed sync history |

The Compat surface intentionally favors shaped compatibility over pretending
to be a full Compat superset. Known limits are tracked in
[`../compat-parity.md`](../compat-parity.md).

### Runtime config bag (`/config`, `/global/config`)

`GET`/`PATCH` **`/config`** and **`/global/config`** expose the same process-local
in-memory JSON object (`GlobalState` starts as `{}`). They are **not** backed
by, loaded from, or written to `config.yaml`.

- **PATCH replaces the entire object** (no deep merge). The payload must be a
  JSON object.
- The only field validation is: if `username` is present, it must be a string.
- All state is lost on process restart.

These routes are for Compat clients that expect a live config bag. Durable
provider/MCP configuration remains on disk via
[`../configuration.md`](../configuration.md) (MCP HTTP routes also do not
durably rewrite `config.yaml`).

### Provider and model catalog

All of the following are derived from the live router catalog, exposed by the
engine as `Engine::provider_catalog()`
([`engine.rs`](../../crates/hya-core/src/engine.rs)), which is
`ProviderRouter::catalog()` flattened/sorted/deduped.

| Method | Path | Response |
| --- | --- | --- |
| `GET` | `/api/provider` | Location-wrapped list of `ProviderInfo` for every provider id in the catalog. |
| `GET` | `/api/provider/:provider_id` | One `ProviderInfo`, or **404** with body `{ "_tag": "ProviderNotFoundError", "providerID", "message" }` when the id is absent. |
| `GET` | `/api/model` | Location-wrapped list of every catalog model as `ModelInfo`. |
| `GET` | `/config/providers` | Legacy shape: `{ providers, default }`. |
| `GET` | `/provider` | Legacy shape: `{ all, default, connected }`. |
| `GET` | `/provider/auth` | Map of provider id → auth methods. **Always** a single `{ "type": "api", "label": "API key" }` entry per provider, regardless of whether the live route uses OAuth. |

Each catalog model is projected with:

- **tools** — from `capabilities.streaming_tool_calls`
- **context** — from `capabilities.max_context` (surfaced on the wire as
  `limit.context` on `ModelInfo`)
- **variants** — the reasoning-variant list, serialized as a keyed JSON object
  (insertion order preserved)

**Empty-catalog fallback.** When `provider_catalog()` is empty, the server
synthesizes one entry from the process agent model (`st.agent.model`), with
`tools: false`, `context: 0`, and **no** variants — so catalog endpoints still
return something offline.

### Permissions

Concrete permission routes
([`compat/permission.rs`](../../crates/hya-server/src/compat/permission.rs)):

| Method | Path | Role |
| --- | --- | --- |
| `GET` | `/permission` | List pending requests (legacy list shape). |
| `POST` | `/permission/:request/reply` | Reply by request id (any session); body parsed for `reply` + optional `message`. |
| `GET` | `/api/permission/request` | List pending requests (location-wrapped). |
| `GET` | `/api/permission/saved` | List SQLite-backed saved permissions (optional `projectID` query). |
| `DELETE` | `/api/permission/saved/:id` | Remove a saved permission. |
| `GET` | `/api/session/:id/permission` | List pending requests for one session. |
| `POST` | `/api/session/:id/permission/:request/reply` | Session-scoped reply. |
| `POST` | `/session/:id/permissions/:request` | Legacy session-scoped reply (`response` field instead of `reply`). |

**Reply vocabulary** (lowercase JSON strings, `rename_all = "lowercase"`):

| Value | Meaning |
| --- | --- |
| `once` | Allow this invocation once. |
| `always` | Allow and remember (saved permission row). |
| `reject` | Deny. |

Modern reply body:

```json
{
  "reply": "reject",
  "message": "do not touch production"
}
```

- `message` is optional. On `reject`, it is forwarded as the permission
  `Decision::Reject { feedback }` and appears in the denial error the model
  sees (`permission denied: … — user says: <feedback>` in
  `hya-tool`). Related auto-replies for the same remember scope do **not**
  copy the feedback (related rejects use `feedback: None`).
- Legacy `POST /session/:id/permissions/:request` uses `{ "response": "once"|"always"|"reject" }`
  with no feedback field.
- Successful modern session reply returns **204 No Content**; root
  `/permission/:request/reply` returns JSON `true`. Unknown request ids return
  404 with a `PermissionNotFoundError` body.

### Workspace adapters

`GET /experimental/workspace/adapter` returns a JSON array of
`WorkspaceAdapterInfo`
([`hya-proto` `workspace.rs`](../../crates/hya-proto/src/workspace.rs)):

```json
{
  "type": "worktree",
  "name": "Worktree",
  "description": "Create a git worktree"
}
```

Wire fields: **`type`**, **`name`**, **`description`** (default empty string).

The handler always includes the built-in `worktree` adapter, then appends
plugin-provided adapters registered on `AppState` / `ServerState`
(`with_workspace_adapters`), skipping any plugin entry whose `type` is already
`worktree`.

## CORS and OpenAPI

The server mirrors request origins and headers globally through
`tower_http::cors`. Compat-compatible OpenAPI discovery is exposed at `/doc`
and `/openapi.json`; it provides implemented path/method skeletons rather than
full request/response schemas.

## Client Crate

[`../../crates/hya-client/src/lib.rs`](../../crates/hya-client/src/lib.rs)
provides a typed reqwest wrapper for the native API:

- `create_session`
- `prompt`
- `events`

The interactive TUI runs in-process through `hya-backend`; the client crate is the
integration surface for code that talks to a running hya server process.
