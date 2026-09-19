# Server and Client

The server lives in [`../../crates/hya-server`](../../crates/hya-server). It
serves exactly one contract — `hya.v1` — over two transports:

- **HTTP/JSON + SSE + WebSocket** (axum): the `/v1` routes generated from
  the `proto/hya/v1` IDL (`crates/hya-api`).
- **gRPC** (tonic): `hya_server::V1Grpc` implements all fifteen generated
  services and dispatches every unary call through the *same* axum `/v1`
  router (protojson in, protojson out, stable error codes mapped from the
  JSON error body), so dual-transport parity holds by construction.
  Serve it with `HYA_GRPC_BIND=host:port`.

Contract references:

- [`../protocol/README.md`](../protocol/README.md) — the integration guide
  (serialization rules, error table, pagination, event-driven model, SSE
  frames, PTY, walkthrough).
- [`../protocol/api-reference.md`](../protocol/api-reference.md) — generated
  per-rpc reference (HTTP binding + gRPC method for every rpc).
- [`../protocol/openapi.json`](../protocol/openapi.json) — generated OpenAPI.
- `proto/hya/v1/*.proto` — the source of truth; regenerate everything with
  `cargo run -p xtask -- gen-api` (vendored protoc, output committed).

## App State

`AppState` holds the shared `SessionEngine`, process agent, pending
permission/question queues, dependency-inverted MCP / Agent-model /
Workflow control handles, workspace adapters, formatter status, and a
catalog-update broadcast. The router wraps it into internal `ServerState`,
adding run tokens for busy/abort behavior plus the process-local config
bag and PTY state. Helper machinery (catalogs, guidance, PTY, worktree,
git) lives under `hya_server::support`.

## The v1 surface

Sixteen services, 79 rpcs: AgentModels (durable per-agent model
preferences), Process (health/location/config/dispose/
upgrade/bootstrap), Catalog (agents/models/providers/commands/skills/
tools), Auth, Session (lifecycle + fork/compact/summarize/revert), Turn
(event-driven admit + get/wait/cancel), Messages + Todo, Events (replay
with `includeRaw` + session/global streams), Interactions (unified
permission/question plane + saved rules), Workflow, Files, Project + VCS,
Worktrees, MCP, Pty (incl. the `StreamPty` bidi bridge), Logs.

Semantics highlights:

- **Event-driven execution**: `CreateTurn` admits and returns a handle;
  progress and terminal state arrive on the streams. `WaitTurn` is a
  convenience for synchronous clients. `SessionInfo.busy` derives from
  the run registry.
- **Reads fold the shared projection**: transcript/todo reads come from
  the event log through `hya_proto::Projection` — no second read model.
  `ListEvents.include_raw` exposes the canonical envelope JSON lines for
  tooling (internal shape documented opaque).
- **Guidance parity**: prompt and command turns compose the session
  agent with AGENTS/reference guidance (`support::reference`), the same
  seam the best legacy path provided.
- **Command expansion**: `/v1` command turns expand through the directory
  command/skill catalog (`support::command_catalog::expand_prompt`),
  falling back to the literal slash for unknown commands. The bootstrap
  catalog snapshot stays stale until the next bootstrap, but command-time
  expansion picks up newly written sources.

## Status codes

Errors render `{"error":{"code","message"}}` with the canonical HTTP
status; gRPC maps the same code to a tonic status. The stable table:
`invalid_argument` 400, `not_found`/`session_not_found` 404,
`permission_denied` 403, `session_busy`/`conflict` 409, `unavailable`
503, `internal` 500. Workflow failures keep their structured
`{"error":{"code","message"}}` codes.

## CORS

`AllowOrigin::mirror_request()`, `AllowHeaders::mirror_request()`,
methods `Any`.

## Clients

- [`../../crates/hya-sdk-v1`](../../crates/hya-sdk-v1) — typed SDK for new
  frontends (HTTP + SSE + `V1SessionMirror`).
- [`../../crates/hya-client`](../../crates/hya-client) — lean typed
  `reqwest` client (tooling, e2e harness).
- `crates/hya-sdk` / `crates/hya-native` — legacy SDK/transport for the
  retired Compat surface (old TUI only; slated for deletion at the
  new-TUI cutover).

## Testing

- `crates/hya-server/tests/v1_api.rs` — v1 HTTP integration suite.
- `crates/hya-server/tests/v1_grpc_parity.rs` — dual-transport
  conformance over a real tonic listener.
- `crates/hya-e2e` (Track P) — the process matrix drives real backends
  entirely through the v1 client.
