# Plugin protocol

Wire contract for native stdio plugins hosted by `hya-plugin`. This is the same
JSON-RPC 2.0 ABI used by AgentBundle sidecars; the framing here is the contract a
plugin author implements.

This document defines the process-plugin wire surface only. The package-level
contribution inventory is broader: one `Bundle` carries optional agents and
Workflow source, tools, Skills, MCP declarations, hooks, and extensions as
prepared payloads. A bundle does not by itself imply a running process.
See [Bundle authoring](agent-bundle-authoring.md)
and [Workflows](workflows.md); those package resources are not additional JSON-RPC
methods.

Sources:
[`crates/hya-plugin/src/protocol.rs`](../crates/hya-plugin/src/protocol.rs),
[`crates/hya-plugin/src/messages.rs`](../crates/hya-plugin/src/messages.rs),
[`crates/hya-plugin/src/client.rs`](../crates/hya-plugin/src/client.rs),
[`crates/hya-plugin/src/host.rs`](../crates/hya-plugin/src/host.rs),
[`crates/hya-plugin/src/dispatcher.rs`](../crates/hya-plugin/src/dispatcher.rs).

Configuration of plugins (YAML / `plugin.toml`) is covered in
[Configuration](configuration.md). Compat/OpenCode JS plugins use this same wire
via the Bun extension adapter (`kind: bun`).

---

## Generated contract

The serde types in `crates/hya-plugin/src/messages.rs` are the source of truth.
The `plugin_hooks!` declaration associates each hook name with its parameter
and reply types and expands the Rust enum and name helpers together. The
existing Rust toolchain generates both adapters' `wire.gen.ts` declarations,
method names, and payload guards, including nested `hya-proto` types. Change the Rust types,
then run from the repository root:

```sh
CARGO_BUILD_JOBS=8 cargo run --locked -p xtask -- gen-plugin
```

Commit the generated files together with the source change. The generator is
deterministic; `cargo test -p xtask` fails if any generated file is stale. The
19-hook JSON goldens under `crates/hya-plugin/tests/fixtures/hook-wire.json` pin
the existing wire forms (including camelCase compaction fields, optional
lineage, result tags, and `chat.params` request headers). Both adapters test
their generated guards against those fixtures. Adapter support remains an
explicit subset of the full native hook contract.

The Bun adapter's `src/stdio.ts` and `src/protocol.ts` are the canonical TS
runtime sources. `gen-plugin` copies them into the Claude adapter, and the
same stale-output test checks those copies. This keeps each installed adapter
self-contained without introducing a shared package or build system. Both
adapters dispatch hook parameters through the generated boundary guards;
invalid request payloads return JSON-RPC `INVALID_PARAMS` (`-32602`).

## Transport

- **Channel:** the host spawns a child process and speaks **newline-delimited JSON**
  (NDJSON) on the child’s **stdin** (host → plugin) and **stdout** (plugin → host).
- Every frame is a JSON object with `"jsonrpc": "2.0"`.
- One JSON object per line; maximum line length is **1 MiB** (see
  [Limits and timeouts](#limits-and-timeouts)).

### Frame classification (`Frame::parse`)

| Shape | Kind |
| --- | --- |
| Has `method` **and** `id` | **Request** (expects a Response) |
| Has `method`, **no** `id` | **Notification** (no reply) |
| Has `result` **xor** `error` (and an `id`) | **Response** |
| Has **both** `result` and `error` | **Rejected** |
| Anything else | Rejected |

---

### TypeScript adapter dispatch

The Bun and Claude adapters share the same stdio implementation. Initialization
is a barrier before dispatching subsequent frames. Independent requests and
notifications then run concurrently; replies correlate by `id` and can arrive
out of order. Hook handlers within one invocation still run in declaration
order. `shutdown` stops admission, drains active calls and observations, then
runs disposal and replies. EOF also drains admitted work. A pending handler
therefore does not block another call, but must finish before shutdown completes.

## Methods

| Method | Direction | Kind | Params | Result |
| --- | --- | --- | --- | --- |
| `initialize` | host → plugin | request / reply | `{ "protocol_version": 1, "host": { "name", "version" } }` | Full plugin declaration (see [Initialize reply](#initialize-reply)) |
| `shutdown` | host → plugin | request / reply | `{}` | `{}` (then process exit) |
| `event` | host → plugin | **notification** (no `id`, no reply) | `{ "envelope": <Envelope> }` | — |
| `tool/call` | host → plugin | request / reply | `{ "tool", "session", "call", "input", "host_capability"? }` | `{ "ok", "output", "time_ms"? }` |
| `api/request` | host → plugin | request / reply | `{ "api", "method", "path", "path_params", "query", "body", "session"?, "call", "host_capability" }` | `{ "status"?, "body"? }` (see [Bundle API endpoints](#bundle-api-endpoints-apirequest)) |
| `host/capability` | plugin → host | request / reply | `{ "capability", "session"?, "call", "method", "params" }` | Handler-defined JSON value or JSON-RPC error |
| `hook/<wire-name>` | host → plugin | request / reply | Hook-specific (see [Hooks](#hooks)) | Hook-specific outcome |

`event` is sent only to plugins that registered the `event` hook. Hook methods
use the literal prefix `hook/` plus the wire name, for example
`hook/tool.execute.before`.

### Request-scoped host capabilities

The host can hand one request an opaque `host_capability` string: a
`tool/call` (`PluginClient::call_tool_with_capability`) or an `api/request`
(`PluginClient::request_api`). A normal `call_tool` request omits it. While
that request is active, the process can send `host/capability` requests on the
same stdio connection, echoing the `session` and `call` it received (a global
`api/request` carries no `session`, so its capability requests omit it too):

```json
{"jsonrpc":"2.0","id":41,"method":"host/capability","params":{"capability":"<host_capability>","session":"<session-id>","call":"<call-id>","method":"example.operation","params":{"value":1}}}
```

`HostCapabilityHandler::handle(method, params)` defines the available operations
for that request and must apply the owning host plane's resource and permission
checks. The transport binds the token to the receiving process connection,
session (none for a global API request), and call id (for an API request, the
synthetic `call` sent in `api/request`). It rejects an unknown, expired, or
cross-request token — including one echoed with the wrong `session` — with
JSON-RPC error `-32001` (`CAPABILITY_DENIED`); malformed params return `-32602`.
The token is revoked when the reply, transport error, timeout, or caller
cancellation ends the request. In-flight host operations are cancelled on
revocation or connection closure. Other child→host request methods still close
the plugin connection.

**Who receives a capability.** Every process started for an installed bundle's
explicit (or implicit JavaScript) `extensions.process` receives one on every
`tool/call` — whatever its kind (`rust`, `bun`, or `claude`) — and on every
`api/request`. Configured plugins (`plugins:` / `plugin.toml`) never do. This
is safe to extend beyond Rust because every operation is read-only or
permission-checked through the calling tool's own permission snapshot, the
token is bound to one connection + session + call, and it dies with the reply;
the bundle process already runs with the user's OS privileges, so the lease
grants no authority beyond reading data about the session that invoked it.
This holds for write methods too: a `POST`/`PUT`/`PATCH`/`DELETE` API request
gets the same read-only operations, so whatever it changes is the bundle's own
state, never hya's.
Adapters that do not use the field (the Bun extension adapter) ignore it.

Operations (`method` values):

| Operation | Tool call | Session API request | Global API request | Params | Result |
| --- | --- | --- | --- | --- | --- |
| `context.describe` | yes | yes | yes | `{}` | Tool call: `{ "request": "tool_call", "session", "parent_session" (nullable), "workdir", "source_tool_call_id", "operation_id" }`. API request: `{ "request": "api", "api", "scope": "session" \| "global", "session" (session scope only), "call" }` |
| `permission.assert` | yes | no (`-32001`) | no (`-32001`) | `{ "action", "resource" }` | `{}`, or `-32002` on denial |
| `session.usage` | yes | yes | no (`-32001`: no session) | `{ "scope"?: "session" \| "tree" \| "root" }` (default `tree`; `root` is tool-call only, `-32602` for an API request) | Usage report (below) |

`permission.assert` accepts lowercase `Action` names and a tagged resource
`{ "kind": "tool|path|glob|command|subagent|url|web_search|skill", "value": string }`
or `{ "kind": "any" }`. It returns `{}` on success, `-32002` on permission
denial, and `-32602` for malformed params. An unsupported operation returns
`-32601`. The host applies the active call's permission snapshot.

`session.usage` reads billed token usage folded from the session event logs
(the same `UsageRecorded` projection fold as the rest of the API; nothing is
appended). It is bound to the lease's session: a tool call reads the calling
session (`session`), the calling session plus every descendant subagent
session (`tree`), or the whole spawn tree of its lineage root (`root`); a
session-scoped API request reads only the requested session (`session`) or its
descendants (`tree`); a global API request has no session to read. Descendants are found through the `members[].child` spawn edges of
each parent log, breadth-first, each session once, at most 512 sessions. The
result:

```json
{
  "session": "<bound session>",
  "scope": "tree",
  "root": "<first row's session>",
  "sessions": [
    {
      "session": "<id>",
      "parent": "<id>",
      "agent": "hya-main",
      "usage": {
        "by_model": {
          "fake/model": {
            "input": 200, "cache_read": 0, "cache_write": 0,
            "output": 40, "reasoning": 10, "reasoning_unknown_output": 0,
            "rounds": 2, "legacy_messages": 0,
            "split": { "thinking": 10, "visible": 30, "unknown": 0 }
          }
        },
        "by_purpose": { "turn": { "...": "same totals shape" } },
        "total": { "...": "same totals shape, summed over models" }
      }
    }
  ],
  "total": { "by_model": {}, "by_purpose": {}, "total": {} },
  "truncated": true
}
```

- `sessions` is breadth-first from `root`; `parent`/`agent` are omitted when
  unknown. `total` merges every row. `truncated` is present (and `true`) only
  when the 512-session cap cut the tree.
- Counters follow the `TokenUsage` invariant: `input` excludes cache reads and
  writes; `output` includes thinking. `split` divides `output` into
  `thinking` + `visible` (calls that reported their thinking share) and
  `unknown` (output of calls that did not, for example Anthropic), so
  `thinking + visible + unknown == output`; clients decide how to present an
  unknown share. `unattributed` keys legacy usage without a serving model.

### Bundle API endpoints (`api/request`)

A bundle with an explicit `extensions.process` may register its own HTTP
endpoints in its manifest (`apis:`, see
[AgentBundle authoring](agent-bundle-authoring.md#api-endpoints-apis)); its
initialize reply must then list exactly those endpoint ids in `apis`. The
server matches a request against the bundle's path templates (session routes
`/v1/sessions/{session}/bundles/{bundle}/…`, global routes
`/v1/bundles/{bundle}/api/…`) and forwards the routed request to the process
of the live runtime generation:

```json
{"jsonrpc":"2.0","id":7,"method":"api/request","params":{"api":"put-item","method":"PUT","path":"/items/a%2Fb","path_params":{"key":"a/b"},"query":{"dry":"1"},"body":{"n":1},"call":"<synthetic-request-id>","host_capability":"<host_capability>"}}
```

| Param | Meaning |
| --- | --- |
| `api` | The matched endpoint id (one of the ids the process declared). |
| `method` | `GET`, `POST`, `PUT`, `PATCH`, or `DELETE`. |
| `path` | The concrete request path below the bundle mount, with its leading `/` and percent-encoding as sent. |
| `path_params` | The template's `{name}` parameters bound by the match, percent-decoded (`{}` when the template has none). |
| `query` | The HTTP query string as a string→string map (may be empty). |
| `body` | The JSON request body, or `null` when the request had none. At most 512 KiB (the host answers `bundle_api_bad_request` before forwarding anything larger). |
| `session` | The session a session-scoped endpoint was called for (it exists). Absent for a global endpoint. |
| `call` | Synthetic request id minted per request; send it back as `host/capability` `call`. |
| `host_capability` | Request-scoped capability (always present). |

The reply is `{ "status"?: 200..=599, "body"?: <any JSON value> }` (unknown
fields rejected; `status` defaults to `200`, `body` to `null`). The server
answers the HTTP caller with that status and the body verbatim as
`application/json`; a `null` body sends no content, and `204`, `205`, and
`304` must not carry a body. A status outside `200..=599`, a body on those
statuses, a JSON-RPC error, a malformed reply, a reply frame over the 1 MiB
stdio cap, a crash, or the ordinary request timeout (30 s, the same as
`tool/call`) becomes API error `bundle_api_failed` (502).

The contract is JSON in, JSON out on purpose: no request headers (in
particular no `Authorization`), no raw bytes, no response headers. That keeps
the gRPC binding at parity (its request and reply carry the same fields as
`google.protobuf.Value`s), keeps the client's credentials away from bundle
code, and leaves one validation point on the host. API endpoints are a
bundle-process feature: configured plugins and the Bun extension adapter do
not serve them.

---

## Error codes

| Code | Name | Meaning |
| --- | --- | --- |
| `-32601` | `METHOD_NOT_FOUND` | Host called a method the plugin does not implement |
| `-32602` | `INVALID_PARAMS` | Malformed params |
| `-32603` | `INTERNAL_ERROR` | Plugin-side failure |
| `1` | `VETO` | App-defined: a guard refused the action |
| `-32001` | `CAPABILITY_DENIED` | Host capability is absent, expired, bound to another request, or the operation is unavailable to this request kind |
| `-32002` | `PERMISSION_DENIED` | The active permission plane denied the requested resource operation |

Guard refusal on the wire is normally a **successful** result with
`"outcome": "veto"` (see `tool.execute.before`). A JSON-RPC error from a
`tool.execute.before` plugin is also treated as a veto by the
host (see [Hook failure behavior](#hook-failure-behavior)). That conversion does **not** apply to
`permission.ask`. Code `1` is the reserved app error constant for an explicit
veto-style RPC error.

---

## Initialize reply

After `initialize`, the plugin must reply with an `InitializeResult`:

```json
{
  "protocol_version": 1,
  "plugin": {
    "id": "memory",
    "version": "0.1.0",
    "kind": "rust"
  },
  "hooks": [
    { "name": "tool.execute.before" },
    { "name": "event" }
  ],
  "tools": [
    {
      "name": "memory_get",
      "description": "Read a memory key",
      "inputSchema": {
        "type": "object",
        "properties": { "key": { "type": "string" } },
        "required": ["key"]
      }
    }
  ],
  "apis": [
    { "name": "usage", "description": "Token usage of the session tree" }
  ]
}
```

| Field | Rules |
| --- | --- |
| `protocol_version` | Must be `1` (`PROTOCOL_VERSION`) or the host aborts with protocol mismatch. |
| `plugin.id` | **Must** equal the configured / manifest id or the host aborts with `IdentityMismatch`. |
| `plugin.version` | Free-form version string. |
| `plugin.kind` | **Required** on the initialize reply (no `#[serde(default)]` on `PluginInfo.kind`). Wire snake_case: `rust`, `bun`, `claude`, `other`. Omitting `kind` fails deserialization and aborts the handshake. (`#[default] Rust` on `PluginKindWire` applies to YAML config / `plugin.toml` entries that do have `#[serde(default)]`, not to this wire field.) |
| `hooks` | Only hooks listed here are ever dispatched to this plugin. Each registration is exactly `{ "name": "<hook>" }`. |
| `tools` | Each entry becomes a first-class hya `Tool`. Field name is camelCase **`inputSchema`**. |
| `workspaceAdapters` | Shape `{ type, name, description }`, parsed but unsupported: no route serves them. A bundle process that declares one fails to start (`process workspace adapters have no bundle resource contract`), and Project plugins and private agent sidecars ignore them with a warning. |
| `apis` | Optional. API endpoints answered over `api/request`: `{ name, description? }`, names unique and non-empty. A bundle process must list exactly its manifest `apis:` ids (otherwise the bundle fails to start); configured plugins' endpoints are ignored. |

---

## Plugin tools

- **`inputSchema.type` must be exactly `"object"`.** Initialization validates
  the complete contribution set and rejects an invalid tool declaration,
  including one outside a consumer's selected view.
- A plugin tool invoked without `ToolCtx.session` fails with
  `plugin tool requires a session`.
- Shared processes receive a fresh `ToolCallId` for every `tool/call`;
  private activations retain the originating tool-call id.
- A reply with `ok: false` becomes a `ToolError` carrying the returned `output`
  stringified.

---

### Bun tool context

A Bun tool's `execute(input, context)` receives only:

```ts
type ToolContext = {
  readonly sessionID: string
  readonly metadata: (input: {
    readonly title?: string
    readonly metadata?: Readonly<Record<string, unknown>>
  }) => void
}
```

`sessionID` is the host's tool-call session. `metadata()` supplies the returned
tool output's title and metadata; it does not publish live updates. For example:

```ts
execute: async (input, context) => {
  context.metadata({ title: "Lookup", metadata: { session: context.sessionID } })
  return "Found the result"
}
```

The fabricated fields `messageID`, `agent`, `directory`, `worktree`, `abort`,
and `ask` have been removed. A process's working directory is its materialized
bundle root, not the session workspace. No cancellation or permission-ask
function is exposed by this facade. Native processes can use the explicitly
granted [request-scoped host capabilities](#request-scoped-host-capabilities).

## Hook failure behavior

Failure handling is fixed by hook class; registrations contain only `name`.
The initialize decoder rejects a registration carrying `posture`. A
`plugin.toml` configures process startup, while the initialize reply is the
sole hook declaration; manifest `[[hooks]]` overrides are rejected.

| Hook class | On call failure, malformed reply, or timeout |
| --- | --- |
| Guard (`tool.execute.before`) | Veto with `guard failed safe: …`. |
| Permissions (`permission.ask`, `permission.approve`) | Defer to the next interceptor or the user. |
| Enrichment, evaluation, and observation | Skip the failed handler and continue with the prior input or built-in behavior. |
| Model fallback (`model.fallback`) | Treat the handler as `give_up`. |

---

## Hooks

Only hooks listed in the initialize reply are dispatched. Each subsection gives
the wire method, params, outcomes, and failure behavior.

### `event` (notification)

- **Method:** `event` (not under `hook/`)
- **Kind:** host → plugin **notification** (no `id`, no reply)
- **Params:** `{ "envelope": <Envelope> }`
- **Failure behavior:** fail-open.
- **Delivery:** best-effort only — see [Event fan-out](#event-fan-out).

### `command.execute.before`

- **Method:** `hook/command.execute.before`
- **Params:** `{ "session", "command", "arguments", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment (folds across plugins)
- **Failure behavior:** fail-open.

### `experimental.text.complete`

- **Method:** `hook/experimental.text.complete`
- **Params:** `{ "session", "message", "part", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment
- **Failure behavior:** fail-open.

### `message.user.before`

- **Method:** `hook/message.user.before`
- **Params:** `{ "session", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment
- **Failure behavior:** fail-open.

### `chat.params`

- **Method:** `hook/chat.params`
- **Params:** `{ "session", "root_session"?, "agent"?, "message", "request": <WireCompletionRequest> }`

  | Field | Type | Meaning |
  | --- | --- | --- |
  | `session` | session id | Session making the completion. |
  | `root_session` | session id, optional | Root of `session`'s spawn tree (the request chain). It equals `session` for a root session; a subagent at any depth reports its top ancestor. The host always sends it; it is optional only so older payloads still decode. |
  | `agent` | string, optional | Stable id of the agent bound to `session` (for example `hya-main`, `hya-scout`, or a bundle agent id). |
  | `message` | message id | Assistant message being prepared. |
  | `request` | `WireCompletionRequest` | The completion request the host intends to send. |

  `root_session` and `agent` are additive: plugins that ignore unknown fields
  keep working. Use `root_session` to keep one decision per request chain (for
  example a model-routing choice shared by a lead and its subagents).
- **`request` fields:** `model`, `system?`, `messages`, `tools`, `temperature?`,
  `max_output_tokens?`, `reasoning?`, `headers` (per-request extra HTTP headers)
- **Outcome:** `{ "outcome": "continue", "request": <WireCompletionRequest> }`
- **Role:** enrichment; a plugin-supplied `reasoning` string that fails to parse
  leaves the **original** effort in place. A rewritten `request.model` is the
  model the turn streams from, and the engine's configured cross-model fallback
  chain for that model still applies.
- **Failure behavior:** fail-open.

Example params for a subagent turn:

```json
{
  "session": "0192f3c4-…-child",
  "root_session": "0192f3c1-…-root",
  "agent": "hya-scout",
  "message": "0192f3c5-…",
  "request": { "model": "anthropic/claude-sonnet-5", "messages": [], "tools": [] }
}
```

### `tool.execute.before` (guard)

- **Method:** `hook/tool.execute.before`
- **Params:** `{ "session", "message", "call", "tool", "input" }`
- **Outcomes:**
  - `{ "outcome": "continue", "input": <rewritten> }`
  - `{ "outcome": "veto", "reason": "<string>" }`
- **Role:** **guard** — first veto short-circuits; later plugins are not called
- On transport/parse failure, host vetoes with reason
  `guard failed safe: <plugin> (<error>)`

### `tool.execute.after`

- **Method:** `hook/tool.execute.after`
- **Params:** `{ "session", "message", "call", "tool", "input", "result" }`
- **`result`:** tagged `WireToolResult` — `{ "status": "ok", "output", "time_ms" }`
  or `{ "status": "err", "message" }`
- **Outcome:** `{ "outcome": "continue", "result": <WireToolResult> }`
- **Role:** enrichment
- **Failure behavior:** fail-open.

### `permission.ask` (permission chain — not a Safe-veto guard)

- **Method:** `hook/permission.ask`
- **Params:** `{ "session"?, "action", "resource" }`
- **`resource`:** tagged `{ "type": "tool"|"path"|"glob"|"command"|"subagent"|"url"|"web_search"|"skill"|"any", "value"? }`
- **Outcomes** (tag `outcome`, snake_case):
  - `allow_once`
  - `allow_always`
  - `reject` with optional `feedback`
  - `defer` (try next plugin / fall through to user ask)
- **Role:** first non-`defer` answer decides
  ([`permission_bridge.rs`](../crates/hya-plugin/src/permission_bridge.rs))
- **Failure policy:** serialize failure, RPC error, or an undecodable reply
  defers to the next registered plugin. If every plugin defers or errors, the
  interceptor returns `None` and the session falls through to the user ask.

### `permission.approve` (session permission mode approver)

Answers permission asks for a session tree that runs in one of the bundle's
declared permission modes (manifest `permission_modes:`, see
[AgentBundle authoring](agent-bundle-authoring.md#permission-modes-permission_modes)).

- **Method:** `hook/permission.approve`
- **Params:**
  `{ "session", "root_session", "agent"?, "mode", "action", "resource" }` —
  `session` is the session whose tool call asks (a subagent session for
  subagent calls), `root_session` the tree root that owns the mode, `agent`
  the stable id of the agent bound to `session`, and `mode` the bundle-local
  mode id (the `permission_modes:` entry's `id`, not the
  `<bundle-id>/<mode-id>` selector). `action` and `resource` are the same as
  for `permission.ask`.
- **Outcomes:** the `permission.ask` outcomes — `allow_once`, `allow_always`
  (remembered like a user's Allow Always), `reject` with optional `feedback`,
  `defer`.
- **Who is asked:** only the process of the bundle that declared the active
  mode, and only after every `permission.ask` interceptor deferred (the
  approver is appended at the end of the chain). It is never called under
  `manual` or `yolo`, and never for checks that rules already allow or deny.
- **Failure policy:** like `permission.ask` — a timeout, RPC error, or
  undecodable reply counts as `defer`, and a final `defer` sends the ask to
  the user. A failing approver therefore never allows anything.

```json
{"jsonrpc":"2.0","id":7,"method":"hook/permission.approve","params":{"session":"hysec_…","root_session":"hysec_…","agent":"hya-main","mode":"careful","action":"bash","resource":{"type":"command","value":"git status"}}}
{"jsonrpc":"2.0","id":7,"result":{"outcome":"allow_once"}}
```

### `model.fallback` (choose the next model before a stream exists)

Lets a plugin pick the next model when a provider fails **before** any event
stream exists. Use it for failover policy the static `categories:` chains
can't express: per-error-class rules, chains per model, or picks based on
the agent or request chain.

- **Method:** `hook/model.fallback`
- **Params:**

  | Field | Type | Meaning |
  | --- | --- | --- |
  | `session` | session id | Session making the completion. |
  | `root_session` | session id | Root of `session`'s spawn tree; equals `session` for a root. |
  | `agent` | string, optional | Stable id of the session's agent. |
  | `message` | message id | Assistant message being prepared. |
  | `model` | string | Model whose attempt just failed (`provider/model`). |
  | `error` | `{ "class", "message" }` | The failure. `class` is one of the values below; `message` is the provider error text. |
  | `attempt` | u32 | 1-based count of failed attempts so far in this round. |
  | `tried` | string[] | Every model attempted in this round, in order. All of them failed. |

  | `error.class` | Provider failures |
  | --- | --- |
  | `retryable` | Transport failure, HTTP 429, HTTP 5xx |
  | `unknown_model` | No provider route claims the model |
  | `auth` | Expired credentials, HTTP 401 or 403 |
  | `invalid_request` | Any other HTTP 4xx, or a route that can't serve the request (for example tools or media it doesn't support) |
  | `other` | Decode, JSON, provider error frame, any other status |

- **Outcomes:** `{ "outcome": "retry", "model": "<provider/model>" }` or
  `{ "outcome": "give_up" }`
- **Role:** decision chain. The first `retry` wins. `give_up` passes the
  consult on to the next plugin.
- **Failure policy:** fail-open:
  an RPC error, a timeout, an undecodable reply, or a `retry` with an empty
  model counts as `give_up` for that plugin.

**When the engine asks.** Each model-selection round first walks the
configured cross-model chain (`categories:`) exactly as it would without the
hook. When that chain can't advance, the engine asks `model.fallback`. That
covers every error class, including ones the chain never advances on, such as
`auth`. A `retry` model is attempted with its own reasoning variant. If it also
fails before a stream exists, the engine asks again, with the new failure and a
longer `tried` list.

**Limits.**

- A `retry` naming a model already in `tried` is refused and ends the round, so
  a plugin can't loop a turn.
- A round makes at most **8** provider attempts in total, counting the
  configured chain.
- The hook is never called once a provider has returned a stream. A mid-stream
  error surfaces once and is not replayed on another model.
- Turns on a Workflow route (`model:` with `fallback:` on a Workflow stage)
  don't call the hook. The Workflow owns its declared candidate list.
- No new events are recorded. Each switch logs a `tracing::warn!` with the
  `from` and `to` models, like the configured chain does.

Example exchange:

```json
{"jsonrpc":"2.0","id":7,"method":"hook/model.fallback","params":{
  "session":"0192f3c4-…","root_session":"0192f3c1-…","agent":"hya-main",
  "message":"0192f3c5-…","model":"anthropic/claude-opus-5-5",
  "error":{"class":"retryable","message":"http status 529: overloaded"},
  "attempt":1,"tried":["anthropic/claude-opus-5-5"]}}
{"jsonrpc":"2.0","id":7,"result":{"outcome":"retry","model":"anthropic/claude-sonnet-5"}}
```

### Injection-point hooks (dispatched, fail-open)

Five observation/enrichment hooks round out the injection surface. All are
**fail-open**: a hook error, timeout, or a plugin
that fails to answer never blocks the engine — the built-in behavior proceeds
and the failure is logged.

| Wire name | Params (camelCase) | Outcome |
| --- | --- | --- |
| `compaction.before` | `{session, trigger, messagesTokenEstimate}`; `trigger` is `"overflow"` or `"proactive"` | `proceed` \| `skip{reason}` \| `replace{instructions}` — `replace` swaps the summarizer instructions; `skip` is honored only for proactive compaction and is demoted to `proceed` when compaction is overflow-forced |
| `compaction.after` | `{session, summaryTokens}` | notification |
| `session.start` | `{session}` | notification |
| `session.end` | `{session}` | notification |
| `agent.spawn` | `{parent, child}` | notification |

### Evaluator hooks (dispatched)

Three hooks turn a plugin into the independent judge for goal/loop mode. All
fail open; the engine's iteration caps and no-progress
detection stay engine-authority and cannot be overridden by a plugin.

| Wire name | Params | Outcome |
| --- | --- | --- |
| `goal.evaluate` | `{condition, transcript}` | `GoalEvaluateReply`: `verdict{met, reason}` or `malformed` — a malformed or failing reply degrades to not-met so a broken evaluator only consumes one iteration of the cap |
| `loop.verifier` | `{target, transcript}` | structured verifier verdict (score/satisfied/evidence/critical gaps) |
| `loop.planner` | `{target, history, last_verdict, planner_notes}` | next directive + continuity brief |

Selection (goal mode): when any registered plugin provides `goal.evaluate`, the
engine uses it as the goal evaluator; otherwise it falls back to the built-in
model evaluator (`--evaluator-model` selects the model, default the worker's
model). See [Goal/loop authoring](goal-loop-authoring.md).

---

## Multiple plugins on one hook

`connect_all_observed` handshakes every plugin **concurrently**, then re-sorts
results by declared index so hook chains always fold in **configured load
order** (config entries first, then directory manifests), regardless of
handshake timing.

| Hook class | Chain rule |
| --- | --- |
| **Enrichment** (`command.execute.before`, `message.user.before`, `experimental.text.complete`, `chat.params`, `tool.execute.after`) | **Fold:** plugin *N*’s output becomes plugin *N+1*’s input. A failing plugin is skipped; its input is passed through. |
| **Guard** (`tool.execute.before`) | **Short-circuit:** first `veto` returns immediately. Call failures become `guard failed safe: <plugin> (<error>)`. |
| **`permission.ask`** | First non-`defer` outcome wins. Serialize/RPC/decode failures **skip** that plugin (no Safe veto). All-defer **or** all-error falls through to the normal user-ask path. |
| **`permission.approve`** | Only the declaring bundle's process is asked; within it, first non-`defer` outcome wins. Failures count as `defer`; all-defer falls through to the user ask. |
| **`model.fallback`** | First `retry` wins. `give_up`, RPC/decode failures, and empty models **skip** that plugin (always fail-open). Configured plugins are asked first, then installed Plugin bundles, then the session agent's own bundle hooks: the same order as `chat.params`. All give-up surfaces the provider error. |

---

## Event fan-out

Each event-subscribing plugin gets its own **256-slot** mpsc channel
(`EVENT_CHANNEL_CAP`). If a plugin is slower than the engine, envelopes are
**dropped** (not queued, not retried). A warning is logged once every
`EVENT_DROP_WARN_EVERY` = **256** drops.

Treat the `event` hook as **best-effort telemetry**. Never use it as the sole
source of truth for plugin state.

---

## Limits and timeouts

These apply to every plugin process (configured plugins and Bundle sidecars):

| Limit | Value | Notes |
| --- | --- | --- |
| `MAX_LINE_BYTES` | **1 MiB** | Exceeding raises `PluginError::OversizedLine` and tears down the transport |
| `DEFAULT_CALL_TIMEOUT` | **30 s** | Per request; overridable per plugin with config `timeout_ms` (milliseconds) |
| `INITIALIZE_TIMEOUT` | **5 s** | Handshake only |
| `SHUTDOWN_TIMEOUT` | **1 s** | After this, the host kills and reaps the child (`start_kill`) |
| `STDERR_TAIL_BYTES` | **64 KiB** | Bundle-spawned children only — last stderr bytes kept for diagnostics |

**Stderr difference:** configured plugins **inherit** the host’s stderr (output
goes straight to the user’s terminal). Bundle sidecars **pipe** stderr into the
bounded tail readable via `ChildGuard::stderr_tail()`.

---

## How the child is spawned

### Configured plugins — `PluginClient::spawn`

- `stdin` / `stdout` piped
- `stderr` **inherited** from the host
- `kill_on_drop` set
- Config `env` map **overlaid** on the host environment (does **not** replace it)

### Bundle sidecars — `PluginClient::spawn_bundle`

- `env_clear()` so the child gets **no** inherited environment
- `cwd` set to the activation directory
- `stderr` piped into the bounded tail
- Strict transport: any timeout **permanently taints** the connection closed

### Plugins spawned in a Project root

`PluginClient::spawn_in(command, cwd, env)` spawns a plugin with the same
environment handling as `PluginClient::spawn` (the host's environment,
overlaid with any explicit config `env`) but with `cwd` as the child's
current directory instead of the host process's own working directory.
The Project loader resolves that directory into each `PluginSpec` before
`PluginHost::connect_all_observed` connects the descriptors in parallel.

This lets a plugin declare a relative `command` (for example a script under
`<project root>/.hya/plugins/<id>/`) that resolves against the Project root
holding it, rather than requiring an absolute path or the server's cwd. A
plugin that crashes and is respawned within its restart budget starts in the
same directory again. Config descriptors retain the host's directory; bundle
descriptors retain their private materialized directory.

Project plugins (`<root>/.hya/plugins/<name>/plugin.toml`) use the `_in`
variants: `hya-app` starts each one in the Project root that holds its
`.hya/plugins` directory, at the Project's first bind (see
[Configuration: Plugins](configuration.md#plugins)). Plugins declared in
`config.yaml` are spawned in the server's working directory as before.

### One descriptor and connection owner

Config plugins, Project plugins, installed/Project `extensions.process`, and
private JavaScript activations all normalize to `hya_plugin::config::PluginSpec`.
`PluginHost` owns initialization, connection state, restart checks, shutdown and
termination through `PluginConn`. Tools and hook chains are views of its complete
initialized declaration; changing a view never changes the handshake.

The Rust descriptor fields are `id: String`, `kind: PluginKindWire`,
`command: Vec<String>`, `timeout_ms: Option<u64>`,
`env: BTreeMap<String, String>`, `plugin_dir: Option<PathBuf>` (resolved by the
Claude launcher), and `launch: ProcessLaunch`:

| Launch | Directory and environment | Loss policy |
| --- | --- | --- |
| `Config` | Host cwd, inherited environment plus `env` | Lazy restart within the existing budget |
| `Project(PathBuf)` | Project root, inherited environment plus `env` | Same restart policy, same root |
| `Bundle { root: PathBuf, activation: None }` | Private root, cleared environment plus `env`, bounded stderr, strict timeouts | Same restart budget and complete-declaration drift check |
| `Bundle { root, activation: Some(ActivationMetadata) }` | Private activation directory, same isolation | Report loss to the session supervisor; never transparently respawn session state |

`PluginHost::connect(spec, host: HostInfo, reads: Option<Arc<dyn HostSessionReads>>)`
returns `Result<PluginHost, PluginError>`. `connect_all_observed(specs, host)`
returns the ordered host and a per-id failure map. `PreparedPlugin::tool(local,
canonical)` returns `Option<Arc<dyn Tool>>` for an exported tool under a view's
canonical name. `PluginHost::shutdown()` and `terminate()` return
`Result<(), PluginError>` after reaping; retained views cannot restart a stopped
owner. `PluginHost::stop_now() -> ()` marks connections stopped and takes their
child guards synchronously, closes clients and kills children immediately,
then reaps on the current runtime. A private sidecar handle calls it on drop
before removing its activation directory, including when tool or hook views
outlive the handle. Calls through retained stopped views return `Closed`.
Dropping a child guard also closes the client and kills synchronously; its tracked
cleanup only reaps and joins I/O, without a graceful RPC or grace-period wait.
Use `shutdown().await` to send the shutdown RPC and await reaping.
`TurnBinding::sidecar_hook_view(agent: &str, dispatcher: Arc<dyn HookDispatcher>)`
returns `Result<Option<Arc<dyn HookDispatcher>>, BundleError>`: no selected hooks
produce `None`; otherwise the retained dispatcher is filtered by `hook_refs`.
`PreparedPlugin::loss_token() -> Option<CancellationToken>` supplies the private
activation's transport-loss signal to its session supervisor.

These are host interfaces, with no new manifest/config fields or wire methods.
Private activations still send the existing `activation_id: String` and
`lifecycle: transient | resident` initialize fields, keep session-local state,
and expose only the agent's selected tools and hooks. Shared processes continue
to initialize and validate every package export before publishing their source.

---

## Atomic source publication and config reload

A configured plugin and a Project plugin use the same publication boundary:
one runtime source owns the connected process, qualified tools, Skills, and
hook dispatcher. The host does not install a second process-wide hook or
permission interceptor. Only a successfully published source can participate
in model, tool, permission, message, and session hooks. Rejected replacements
leave the previous source effective; an unrelated failed plugin does not
prevent a ready source from publishing.

To reload a configured plugin, edit its `plugins.<id>` entry in `config.yaml`
and start another turn, or call `POST /v1/bundles:refresh` with
`{"directory":""}` (`hya.v1.Catalog/RefreshBundles`). For example, changing
`command: [python3, /absolute/path/plugin.py]` to a new command prepares that
plugin and atomically replaces its tools and hooks. Removing the entry or
setting its boolean `enabled` field to `false` removes the source. The daemon
PID stays the same. Active turn bindings retain the old process until released;
idle sessions do not pin removed global plugins. Global event and lifecycle
hooks use the current published global sources independently of Project cache
eviction. Every live envelope reaches global `event` hooks exactly once,
including child prompt admission while a sidecar is active, before the first
assistant event. Activation chains retain global plugins for ordered non-event
hooks while omitting their event delivery; captured session chains contain only
scoped hooks. Non-event hook and `permission.ask` order is user-global config
plugins, installed plugin bundles, then project plugins.

Automatic binds do not retry unchanged failed declarations. Explicit refresh
retries failures, including a repaired plugin executable with unchanged
`plugin.toml` or `config.yaml`. Project plugins keep their last published tools
and hooks after malformed manifests or failed initialization, while independent
sources can publish. The existing initialize and hook request/response schemas are unchanged;
refresh failures appear in the refresh response's `errors` array (see
[Bundle runtime](bundle-runtime.md)).

---

## Supervision and restart budget

| Constant | Value |
| --- | --- |
| `MAX_RESTARTS` | **3** |
| `RESTART_WINDOW` | **60 s** sliding window |

Exceeding the budget sets `disabled` **permanently** for the rest of the host
process lifetime. Later calls return `PluginError::Disabled`; there is no
automatic re-enable — the user must restart hya.

Observable state via `PluginHost::plugin_status(id)`:

| `PluginStatus` | Meaning |
| --- | --- |
| `Alive` | Open live client present |
| `Dead` | Client absent or closed; next shared-source call may lazily respawn |
| `DeclarationDrift` | Respawned child changed its initialize declaration; latched, never reused |
| `Disabled` | Restart budget exhausted |

A crash (EOF mid-call) clears the live client (`Dead`); the next call charges
the restart budget and respawns if still under the window.

Concurrent failed calls invalidate only the client they used. One crashed child
costs one restart even with multiple in-flight calls; a closed or timeout-tainted
client is replaced before the next shared-source dispatch. Private activations
remain non-respawning and report process loss to their session supervisor.

---

## Minimal example plugin (Python)

Answers `initialize` and enriches `message.user.before`. Run as:

```yaml
plugins:
  example:
    command: [python3, /path/to/example_plugin.py]
```

```python
import json
import sys

def reply(id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": id, "result": result}), flush=True)

def error(id, code, message):
    print(
        json.dumps(
            {
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": code, "message": message},
            }
        ),
        flush=True,
    )

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    rid = msg.get("id")

    # Notifications have no id — ignore for this minimal example.
    if rid is None:
        continue

    if method == "initialize":
        reply(
            rid,
            {
                "protocol_version": 1,
                "plugin": {"id": "example", "version": "0.0.1", "kind": "other"},
                "hooks": [{"name": "message.user.before"}],
                "tools": [],
                "workspaceAdapters": [],
            },
        )
    elif method == "shutdown":
        reply(rid, {})
        break
    elif method == "hook/message.user.before":
        text = msg.get("params", {}).get("text", "")
        reply(rid, {"outcome": "continue", "text": text + "\n[example plugin]"})
    else:
        error(rid, -32601, f"method not found: {method}")
```

---

## Shared stdio framing

Native plugins and MCP stdio clients use `hya-mcp::codec` for NDJSON reads
and writes. This keeps the 1 MiB input limit consistent: the limit includes
any newline and is enforced before extending the frame buffer, even when the
peer never sends a newline. Exceeding it fails the connection and its pending
calls with an explicit oversized-line error.

For example, an MCP server may send
`{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}` followed by
a response on a separate line. MCP clients ignore these one-way notifications
and continue matching responses by numeric `id`; notifications do not refresh
the catalog automatically. Plugin notification dispatch is unchanged.

The shared Rust interface is `MAX_LINE_BYTES: usize = 1048576`,
`read_bounded_line<R: AsyncRead + Unpin>(&mut BufReader<R>, &mut Vec<u8>)
-> Result<usize, FrameError>` (zero bytes at EOF), and
`write_frame<W: AsyncWrite + Unpin, T: Serialize>(&mut W, &T)
-> Result<(), FrameError>` (JSON, newline, flush).
`FrameError` is `Io(String) | Json(String) | OversizedLine`.
MCP maps these to `McpError`; plugins retain `PluginError`.
Dropping an MCP child guard terminates through `Child::start_kill` and reaps
with `Child::wait` on the active runtime, without spawning a kill utility.


## Related

- [Configuration](configuration.md) — `plugins:` YAML and `plugin.toml`
- [Agent bundle authoring](agent-bundle-authoring.md) — sidecar lifecycle framing
- [Runtime architecture](architecture/runtime.md) — how the engine drives hooks

### Activation process adapter

`hya_plugin::activation` owns private activation process startup and lifetime.
It creates the activation directory, connects through `PluginHost`, terminates a
process whose exports fail validation, and returns a core `SidecarHandle` with
the selected tools/hooks and the process loss token. Shutdown and termination
remove the directory after stopping the process; drop stops it immediately.
This uses the existing initialize, tool and hook wire contracts above.

Hosts call the async
`start_activation_sidecar(start: SidecarStart, command: &[String], staging_root: &Path,
host_info: HostInfo, io_tasks: Option<TaskTracker>, launch, bind)
-> Result<Box<dyn SidecarHandle>, CoreError>`.
`launch: FnOnce(&SidecarStart, &Path) -> Result<PluginSpec, CoreError>` composes the
bound command, materialized resources and application configuration.
`bind: FnOnce(&PreparedPlugin, Arc<PluginHost>) -> Result<ActivationBindings, CoreError>`
validates the complete declaration and selects its agent-visible view.
`ActivationBindings` contains `tools: Arc<[ResolvedTool]>` and
`hooks: Option<Arc<dyn HookDispatcher>>`.
For example, app composition supplies the captured bundle resource closure and
configuration-file environment in `launch`, then validates and binds that
catalog's tool/hook selections in `bind`. The plugin adapter retains the sole
connection owner. No new command, configuration key, or wire message is needed.

The adapter's captured-resource helpers are
`materialize_bundle_sidecar_resources(&TurnBinding, &str, &Path) -> Result<Vec<PathBuf>, CoreError>`,
`bind_bundle_sidecar_tools(&TurnBinding, &str, &PreparedPlugin) -> Result<Arc<[ResolvedTool]>, CoreError>`,
and `validate_bundle_sidecar_hooks(&TurnBinding, &str, &[HookRegistration]) -> Result<(), CoreError>`.
The string argument is the stable agent identity. Materialization uses only the
captured resource closure; tool/hook validation checks the complete loaded
module declaration before selecting the agent-visible exports.
