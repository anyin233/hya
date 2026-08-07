# Plugin protocol

Wire contract for native stdio plugins hosted by `hya-plugin`. This is the same
JSON-RPC 2.0 ABI used by AgentBundle sidecars; the framing here is the contract a
plugin author implements.

Sources:
[`crates/hya-plugin/src/protocol.rs`](../crates/hya-plugin/src/protocol.rs),
[`crates/hya-plugin/src/messages.rs`](../crates/hya-plugin/src/messages.rs),
[`crates/hya-plugin/src/client.rs`](../crates/hya-plugin/src/client.rs),
[`crates/hya-plugin/src/host.rs`](../crates/hya-plugin/src/host.rs),
[`crates/hya-plugin/src/dispatcher.rs`](../crates/hya-plugin/src/dispatcher.rs).

Configuration of plugins (YAML / `plugin.toml`) is covered in
[Configuration](configuration.md). Compat/OpenCode JS plugins use this same wire
via the Bun adapter; see [Compat plugins](compat-plugins.md).

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
| `tool/call` | host → plugin | request / reply | `{ "tool", "session", "call", "input" }` | `{ "ok", "output", "time_ms"? }` |
| `hook/<wire-name>` | host → plugin | request / reply | Hook-specific (see [Hooks](#hooks)) | Hook-specific outcome |

`event` is sent only to plugins that registered the `event` hook. Hook methods
use the literal prefix `hook/` plus the wire name, for example
`hook/tool.execute.before`.

---

## Error codes

| Code | Name | Meaning |
| --- | --- | --- |
| `-32601` | `METHOD_NOT_FOUND` | Host called a method the plugin does not implement |
| `-32602` | `INVALID_PARAMS` | Malformed params |
| `-32603` | `INTERNAL_ERROR` | Plugin-side failure |
| `1` | `VETO` | App-defined: a guard refused the action |

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
  ]
}
```

| Field | Rules |
| --- | --- |
| `protocol_version` | Must be `1` (`PROTOCOL_VERSION`) or the host aborts with protocol mismatch. |
| `plugin.id` | **Must** equal the configured / manifest id or the host aborts with `IdentityMismatch`. |
| `plugin.version` | Free-form version string. |
| `plugin.kind` | **Required** on the initialize reply (no `#[serde(default)]` on `PluginInfo.kind`). Wire snake_case: `rust`, `compat`, `other`. Alias `opencode` is accepted for `compat`. Omitting `kind` fails deserialization and aborts the handshake. (`#[default] Rust` on `PluginKindWire` applies to YAML config / `plugin.toml` entries that do have `#[serde(default)]`, not to this wire field.) |
| `hooks` | Only hooks listed here are ever dispatched to this plugin. Optional per-hook `posture`. |
| `tools` | Each entry becomes a first-class hya `Tool`. Field name is camelCase **`inputSchema`**. |
| `workspaceAdapters` | Aggregated across all loaded plugins and served verbatim at `GET /experimental/workspace/adapter`. Shape: `{ type, name, description }`. |

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
- **Params:** `{ "session", "message", "request": <WireCompletionRequest> }`
- **`request` fields:** `model`, `system?`, `messages`, `tools`, `temperature?`,
  `max_output_tokens?`, `reasoning?`, `headers` (per-request extra HTTP headers)
- **Outcome:** `{ "outcome": "continue", "request": <WireCompletionRequest> }`
- **Role:** enrichment; a plugin-supplied `reasoning` string that fails to parse
  leaves the **original** effort in place
- **Default posture:** Open

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

### Dead hooks (registered but never dispatched)

These names parse from `plugin.toml` and from the initialize reply and can be
stored on the connection, but
[`dispatcher.rs`](../crates/hya-plugin/src/dispatcher.rs) has **no** dispatch
arm for them. They are **never** called:

| Wire name |
| --- |
| `goal.evaluate` |
| `loop.verifier` |
| `loop.planner` |

Do not build a plugin that depends on these hooks.

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

- [Compat plugins](compat-plugins.md) — OpenCode/Compat adapter over this wire
- [Configuration](configuration.md) — `plugins:` YAML and `plugin.toml`
- [Agent bundle authoring](agent-bundle-authoring.md) — sidecar lifecycle framing
- [Runtime architecture](architecture/runtime.md) — how the engine drives hooks
