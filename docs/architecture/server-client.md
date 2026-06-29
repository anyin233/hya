# Server and Client

The server lives in [`../../crates/hya-server`](../../crates/hya-server). It
wraps `SessionEngine` with Axum routes, native SSE streams, and
OpenCode-compatible HTTP/SSE route groups.

## App State

`AppState` contains:

- shared `SessionEngine`
- process-level `AgentSpec`
- pending permission/question queues
- configured MCP manager
- workspace adapter metadata
- formatter status

The router wraps it into internal `ServerState`, which adds run tokens for
busy/abort behavior plus process-local global, MCP HTTP, project, PTY, and TUI
state used by compatibility routes. The native routes run prompts through
the server's configured `AgentSpec`. OpenCode-compatible routes translate
OpenCode-shaped request/response bodies to the same engine, event log,
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
`hysec_...`, `ses_...`, or legacy raw UUID. Invalid ids return `400 Bad
Request`. Busy sessions return `409 Conflict`. Runtime errors are returned as
`500 Internal Server Error` unless a compatibility route maps the error into a
typed OpenCode body.

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

`POST /sessions/:id/prompt` admits a user prompt, runs one assistant turn, and
returns the user message id plus finish reason. `command` records command
metadata before running a turn. `shell` runs the shell tool directly and records
a synthetic assistant tool-result message.

## Native Events

`GET /sessions/:id/events` replays stored envelopes for a session. Use
`?since_seq=<n>` to receive only envelopes whose sequence is greater than `n`.

`GET /sessions/:id/stream` subscribes to the engine event bus and emits SSE
events for the requested session. If the broadcast receiver lags, the server
emits an SSE event named `resync`; clients should use the events endpoint with
their last seen sequence to catch up.

## OpenCode-Compatible Route Groups

`opencode::router()` is merged into the same Axum app. Current route groups
include:

| Group | Examples | Backing implementation |
| --- | --- | --- |
| Sessions | `/session`, `/session/:id`, `/api/session`, `/api/session/:id/context`, `/api/session/:id/message`, prompt/command/shell/abort/fork/share/update/delete/revert/summarize routes | hya event log, projection, run registry, switch/session-state events, pending queues |
| Events | `/event`, `/api/event`, `/global/event` | translated live hya envelopes plus OpenCode heartbeat/connected/status/error frames |
| Files/search | `/file`, `/file/content`, `/find`, `/find/file`, `/find/symbol`, `/api/fs/read/*path`, `/api/fs/list`, `/api/fs/find` | filesystem reads, ignore matching, MIME sniffing, fuzzy path search, optional `LspPlane` |
| Catalogs/metadata | `/path`, `/agent`, `/command`, `/skill`, `/lsp`, `/formatter`, `/api/location`, `/api/agent`, `/api/command`, `/api/skill` | built-in catalog sources, prompt directories, local skills, formatter/LSP planes |
| Provider/auth | `/config`, `/config/providers`, `/provider`, `/provider/auth`, `/auth/:providerID`, `/api/provider`, `/api/model`, credential/integration routes | resolved hya provider catalog and local auth token store |
| Permissions/questions | `/permission`, `/question`, `/api/permission/*`, `/api/question/*`, session-scoped pending queues | hya ask/question channels and SQLite-backed saved permissions |
| MCP | `/mcp`, `/mcp/:name/connect`, `/mcp/:name/disconnect`, auth routes | configured MCP manager plus dynamic in-process status compatibility |
| PTY | `/pty/*`, `/api/pty/*` | in-process PTY metadata and websocket shell attach lifecycle |
| VCS/project/worktree | `/vcs/*`, `/project/*`, `/experimental/project/*/copy`, `/experimental/worktree/*` | git commands, project state, git worktree helpers |
| TUI/global/sync/experimental | `/tui/*`, `/global/*`, `/sync/*`, `/experimental/*` | process-local compatibility queues/state and event-log-backed sync history |

The OpenCode surface intentionally favors shaped compatibility over pretending
to be a full OpenCode superset. Known limits are tracked in
[`../opencode-parity.md`](../opencode-parity.md).

## CORS and OpenAPI

The server mirrors request origins and headers globally through
`tower_http::cors`. OpenCode-compatible OpenAPI discovery is exposed at `/doc`
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
