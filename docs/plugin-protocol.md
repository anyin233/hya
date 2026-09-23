# Plugin protocol

Wire contract for native stdio plugins hosted by `hya-plugin`. This is the same
JSON-RPC 2.0 ABI used by AgentBundle sidecars; the framing here is the contract a
plugin author implements.

This document defines the process-plugin wire surface only. The package-level
contribution inventory is broader: `Plugin` carries agentless resources;
`AgentBundle`, `AgentSetBundle`, and `WorkflowBundle` additionally carry agents
and, for WorkflowBundle, Workflow source. Tools, Skills, MCP declarations, hooks,
and extensions use prepared payloads. The package kind `Plugin` does not by
itself imply a running process. See [AgentBundle authoring](agent-bundle-authoring.md)
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

## Methods

| Method | Direction | Kind | Params | Result |
| --- | --- | --- | --- | --- |
| `initialize` | host → plugin | request / reply | `{ "protocol_version": 1, "host": { "name", "version" } }` | Full plugin declaration (see [Initialize reply](#initialize-reply)) |
| `shutdown` | host → plugin | request / reply | `{}` | `{}` (then process exit) |
| `event` | host → plugin | **notification** (no `id`, no reply) | `{ "envelope": <Envelope> }` | — |
| `tool/call` | host → plugin | request / reply | `{ "tool", "session", "call", "input", "host_capability"? }` | `{ "ok", "output", "time_ms"? }` |
| `view/get` | host → plugin | request / reply | `{ "view", "session", "call", "query", "host_capability" }` | `{ "body": <any JSON> }` (see [Session views](#session-views-viewget)) |
| `host/capability` | plugin → host | request / reply | `{ "capability", "session", "call", "method", "params" }` | Handler-defined JSON value or JSON-RPC error |
| `hook/<wire-name>` | host → plugin | request / reply | Hook-specific (see [Hooks](#hooks)) | Hook-specific outcome |

`event` is sent only to plugins that registered the `event` hook. Hook methods
use the literal prefix `hook/` plus the wire name, for example
`hook/tool.execute.before`.

### Request-scoped host capabilities

The host can hand one request an opaque `host_capability` string: a
`tool/call` (`PluginClient::call_tool_with_capability`) or a `view/get`
(`PluginClient::get_view`). A normal `call_tool` request omits it. While that
request is active, the process can send `host/capability` requests on the same
stdio connection, echoing the `session` and `call` it received:

```json
{"jsonrpc":"2.0","id":41,"method":"host/capability","params":{"capability":"<host_capability>","session":"<session-id>","call":"<call-id>","method":"example.operation","params":{"value":1}}}
```

`HostCapabilityHandler::handle(method, params)` defines the available operations
for that request and must apply the owning host plane's resource and permission
checks. The transport binds the token to the receiving process connection,
session, and call id (for a view request, the synthetic `call` sent in
`view/get`). It rejects an unknown, expired, or cross-request token with
JSON-RPC error `-32001` (`CAPABILITY_DENIED`); malformed params return `-32602`.
The token is revoked when the reply, transport error, timeout, or caller
cancellation ends the request. In-flight host operations are cancelled on
revocation or connection closure. Other child→host request methods still close
the plugin connection.

**Who receives a capability.** Every process started for an installed bundle's
explicit (or implicit JavaScript) `extensions.process` receives one on every
`tool/call` — whatever its kind (`rust`, `bun`, or `claude`) — and on every
`view/get`. Configured plugins (`plugins:` / `plugin.toml`) never do. This is
safe to extend beyond Rust because every operation is read-only or
permission-checked through the calling tool's own permission snapshot, the
token is bound to one connection + session + call, and it dies with the reply;
the bundle process already runs with the user's OS privileges, so the lease
grants no authority beyond reading data about the session that invoked it.
Adapters that do not use the field (the Bun extension adapter) ignore it.

Operations (`method` values):

| Operation | Tool call | View request | Params | Result |
| --- | --- | --- | --- | --- |
| `context.describe` | yes | yes | `{}` | Tool call: `{ "request": "tool_call", "session", "parent_session" (nullable), "workdir", "source_tool_call_id", "operation_id" }`. View: `{ "request": "view", "session", "view", "call" }` |
| `permission.assert` | yes | no (`-32001`) | `{ "action", "resource" }` | `{}`, or `-32002` on denial |
| `session.usage` | yes | yes | `{ "scope"?: "session" \| "tree" \| "root" }` (default `tree`; `root` is tool-call only, `-32602` for a view) | Usage report (below) |

`permission.assert` accepts lowercase `Action` names and a tagged resource
`{ "kind": "tool|path|glob|command|subagent|url|web_search|skill", "value": string }`
or `{ "kind": "any" }`. It returns `{}` on success, `-32002` on permission
denial, and `-32602` for malformed params. An unsupported operation returns
`-32601`. The host applies the active call's permission snapshot.

`session.usage` reads billed token usage folded from the session event logs
(the same `UsageRecorded` projection fold as the rest of the API; nothing is
appended). It is bound to the lease's session: a tool call reads the calling
session (`session`), the calling session plus every descendant subagent
session (`tree`), or the whole spawn tree of its lineage root (`root`); a view
request reads only the requested session (`session`) or its descendants
(`tree`). Descendants are found through the `members[].child` spawn edges of
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
      "agent": "build",
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

### Session views (`view/get`)

A bundle with an explicit `extensions.process` may declare read-only session
views in its manifest (`views:`, see
[AgentBundle authoring](agent-bundle-authoring.md#session-views-views)); its
initialize reply must then list exactly those ids in `views`. The server's
`GET /v1/sessions/{session}/views/{bundle}/{view}` forwards to the process of
the live runtime generation:

```json
{"jsonrpc":"2.0","id":7,"method":"view/get","params":{"view":"usage","session":"<session-id>","call":"<synthetic-request-id>","query":{"scope":"tree"},"host_capability":"<host_capability>"}}
```

| Param | Meaning |
| --- | --- |
| `view` | A declared view id. |
| `session` | The session the view is read for (it exists). |
| `call` | Synthetic request id minted per view request; send it back as `host/capability` `call`. |
| `query` | The HTTP query string as a string→string map, verbatim (may be empty). |
| `host_capability` | Request-scoped capability (always present). |

The reply is `{ "body": <any JSON value> }` (unknown fields rejected). The host
serves it as `application/json` unchanged. A JSON-RPC error, a malformed
reply, a crash, or the ordinary request timeout (30 s, the same as
`tool/call`) becomes API error `view_failed`. Views are a
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
**Safe**-posture `tool.execute.before` plugin is also treated as a veto by the
host (see [Hook posture](#hook-posture)). That conversion does **not** apply to
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
    { "name": "tool.execute.before", "posture": "safe" },
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
  "workspaceAdapters": [
    {
      "type": "example",
      "name": "Example adapter",
      "description": "Surfaced at GET /experimental/workspace/adapter"
    }
  ],
  "views": [
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
| `hooks` | Only hooks listed here are ever dispatched to this plugin. Optional per-hook `posture`. |
| `tools` | Each entry becomes a first-class hya `Tool`. Field name is camelCase **`inputSchema`**. |
| `workspaceAdapters` | Aggregated across all loaded plugins and served verbatim at `GET /experimental/workspace/adapter`. Shape: `{ type, name, description }`. |
| `views` | Optional. Read-only session views answered over `view/get`: `{ name, description? }`, names unique and non-empty. A bundle process must list exactly its manifest `views:` ids (otherwise the bundle fails to start); configured plugins' views are ignored. |

---

## Plugin tools

- **`inputSchema.type` must be exactly `"object"`.** `PluginTool::try_new`
  **silently drops** any declared tool whose `inputSchema.type` is not the
  string `"object"`. The tool never reaches the model and no error is raised —
  authors only see a missing tool.
- A plugin tool invoked without `ToolCtx.session` fails with
  `plugin tool requires a session`.
- The host mints a fresh `ToolCallId` for every `tool/call`.
- A reply with `ok: false` becomes a `ToolError` carrying the returned `output`
  stringified.

---

## Hook posture

Posture is the **per-hook failure policy**. Wire values (serde snake_case):
`safe` and `open`.

| Posture | On hook call failure or timeout |
| --- | --- |
| **Safe** | For **`tool.execute.before` only**, the host converts transport/parse failure into a **veto** (`guard failed safe: …`). Other hooks that declare Safe (including `permission.ask`) do **not** get that conversion — see each hook. |
| **Open** | Failure is logged / skipped; the pipeline continues with the prior input. |

**Defaults** (`HookName::default_posture`):

| Hooks | Default |
| --- | --- |
| `permission.ask`, `tool.execute.before` | **Safe** |
| Every other hook | **Open** |

**Tightening only:** `force_safer(declared, default)` ORs postures so that if
either the declared posture or the hook default is Safe, the effective posture
is Safe. A plugin that declares `open` on a Safe-by-default hook still runs
**Safe**.

**Resolution order** for each registered hook
([`host/connection.rs`](../crates/hya-plugin/src/host/connection.rs)):

1. `posture` in the initialize reply (if present)
2. Else the manifest `posture_overrides` entry for that hook name
3. Else `HookName::default_posture()`
4. Then `force_safer` against the hook default

---

## Hooks

Only hooks listed in the initialize reply are dispatched. Each subsection gives
the wire method, params, outcomes, and default posture.

### `event` (notification)

- **Method:** `event` (not under `hook/`)
- **Kind:** host → plugin **notification** (no `id`, no reply)
- **Params:** `{ "envelope": <Envelope> }`
- **Default posture:** Open
- **Delivery:** best-effort only — see [Event fan-out](#event-fan-out).

### `command.execute.before`

- **Method:** `hook/command.execute.before`
- **Params:** `{ "session", "command", "arguments", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment (folds across plugins)
- **Default posture:** Open

### `experimental.text.complete`

- **Method:** `hook/experimental.text.complete`
- **Params:** `{ "session", "message", "part", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment
- **Default posture:** Open

### `message.user.before`

- **Method:** `hook/message.user.before`
- **Params:** `{ "session", "text" }`
- **Outcome:** `{ "outcome": "continue", "text": "<rewritten>" }`
- **Role:** enrichment
- **Default posture:** Open

### `chat.params`

- **Method:** `hook/chat.params`
- **Params:** `{ "session", "root_session"?, "agent"?, "message", "request": <WireCompletionRequest> }`

  | Field | Type | Meaning |
  | --- | --- | --- |
  | `session` | session id | Session making the completion. |
  | `root_session` | session id, optional | Root of `session`'s spawn tree (the request chain). It equals `session` for a root session; a subagent at any depth reports its top ancestor. The host always sends it; it is optional only so older payloads still decode. |
  | `agent` | string, optional | Stable id of the agent bound to `session` (for example `build`, `explore`, or a bundle agent id). |
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
- **Default posture:** Open

Example params for a subagent turn:

```json
{
  "session": "0192f3c4-…-child",
  "root_session": "0192f3c1-…-root",
  "agent": "explore",
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
- **Default posture:** Safe
- On Safe-posture transport/parse failure, host vetoes with reason  
  `guard failed safe: <plugin> (<error>)`

### `tool.execute.after`

- **Method:** `hook/tool.execute.after`
- **Params:** `{ "session", "message", "call", "tool", "input", "result" }`
- **`result`:** tagged `WireToolResult` — `{ "status": "ok", "output", "time_ms" }`
  or `{ "status": "err", "message" }`
- **Outcome:** `{ "outcome": "continue", "result": <WireToolResult> }`
- **Role:** enrichment
- **Default posture:** Open

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
- **Default posture:** Safe (registration default only)
- **Failure policy:** posture is **not** a veto switch here. The host only
  skips plugins with no registered posture; serialize failure, RPC error, or
  undecodable reply all `continue` to the next plugin. If every plugin defers
  or errors, the interceptor returns `None` and the session falls through to
  the interactive user ask. Contrast `tool.execute.before`, the sole hook
  where Safe posture turns a call failure into a veto
  (`GUARD_FAILED_SAFE` in
  [`dispatcher.rs`](../crates/hya-plugin/src/dispatcher.rs)).

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
- **Default posture:** Open. The hook always fails open, whatever the posture:
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
  "session":"0192f3c4-…","root_session":"0192f3c1-…","agent":"build",
  "message":"0192f3c5-…","model":"anthropic/claude-opus-5-5",
  "error":{"class":"retryable","message":"http status 529: overloaded"},
  "attempt":1,"tried":["anthropic/claude-opus-5-5"]}}
{"jsonrpc":"2.0","id":7,"result":{"outcome":"retry","model":"anthropic/claude-sonnet-5"}}
```

### Injection-point hooks (dispatched, fail-open)

Five observation/enrichment hooks round out the injection surface. All default
to posture **Open** and are **fail-open**: a hook error, timeout, or a plugin
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
default to posture **Open**; the engine's iteration caps and no-progress
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
| **Enrichment** (`command.execute.before`, `message.user.before`, `experimental.text.complete`, `chat.params`, `tool.execute.after`) | **Fold:** plugin *N*’s output becomes plugin *N+1*’s input. A failing Open-posture plugin is skipped; its input is passed through. |
| **Guard** (`tool.execute.before`) | **Short-circuit:** first `veto` returns immediately. Safe-posture failures become `guard failed safe: <plugin> (<error>)`. |
| **`permission.ask`** | First non-`defer` outcome wins. Serialize/RPC/decode failures **skip** that plugin (no Safe veto). All-defer **or** all-error falls through to the normal user-ask path. |
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
| `Alive` | Live client present |
| `Dead` | Client cleared; next call may lazily respawn |
| `DeclarationDrift` | Respawned child changed its initialize declaration; latched, never reused |
| `Disabled` | Restart budget exhausted |

A crash (EOF mid-call) clears the live client (`Dead`); the next call charges
the restart budget and respawns if still under the window.

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

## Related

- [Configuration](configuration.md) — `plugins:` YAML and `plugin.toml`
- [Agent bundle authoring](agent-bundle-authoring.md) — sidecar lifecycle framing
- [Runtime architecture](architecture/runtime.md) — how the engine drives hooks
