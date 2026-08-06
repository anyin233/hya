# Compat plugins

How hya runs OpenCode/Compat-style JavaScript plugins through the bundled Bun
adapter (`crates/hya-plugin-compat/adapter`). The adapter speaks the host
[plugin protocol](plugin-protocol.md) on stdio and translates OpenCode hook
names, tools, and events into hya wire shapes.

Targeted package versions (pinned in Rust at
[`crates/hya-plugin-compat/src/lib.rs`](../crates/hya-plugin-compat/src/lib.rs)):

| Package | Version |
| --- | --- |
| `@opencode-ai/plugin` | **1.17.9** |
| `@opencode-ai/sdk` | **1.17.9** |

Check this pin before assuming a newer OpenCode SDK feature is available.

---

## Adapter CLI

The adapter entrypoint is `bun run src/main.ts` (or the path hya-app spawns for
`kind: compat` plugins).

| Argument | Effect |
| --- | --- |
| `--help` / `-h` | Print usage and exit |
| `--version` | Print adapter version and exit |
| `--bundle-extension <absolute-path>` | **Repeatable.** Registers a materialized JS entrypoint. Path **must be absolute**; relative paths are rejected. |
| Leading bare `--` | Optional separator; stripped before parsing |

AgentBundle sidecars hand the adapter their selected JS entrypoints this way:
hya-app appends `-- --bundle-extension <abs path>` once per selected entrypoint
when launching the sidecar.

---

## Methods the adapter answers

| Method | Notes |
| --- | --- |
| `initialize` | Load plugins, return declaration |
| `shutdown` | Run dispose hooks, then reply and exit |
| `tool/call` | Registered compat tools |
| `hook/message.user.before` | |
| `hook/chat.params` | |
| `hook/command.execute.before` | |
| `hook/experimental.text.complete` | |
| `hook/permission.ask` | |
| `hook/tool.execute.before` | Also drives `shell.env` (see quirks) |
| `hook/tool.execute.after` | |
| `event` | Id-less notification |

Anything else returns JSON-RPC **`METHOD_NOT_FOUND`** (`-32601`).

---

## OpenCode → hya hook translation

Mapping is **many-to-one** and therefore lossy. After mapping, **duplicate hya
names collapse to one registration** (first match wins in adapter registration
order).

| OpenCode hook | hya wire name |
| --- | --- |
| `event` | `event` |
| `command.execute.before` | `command.execute.before` |
| `experimental.text.complete` | `experimental.text.complete` |
| `chat.message` | `message.user.before` |
| `chat.params` | `chat.params` |
| `chat.headers` | `chat.params` |
| `experimental.chat.messages.transform` | `chat.params` |
| `experimental.chat.system.transform` | `chat.params` |
| `tool.definition` | `chat.params` |
| `permission.ask` | `permission.ask` |
| `shell.env` | `tool.execute.before` |
| `tool.execute.before` | `tool.execute.before` |
| `tool.execute.after` | `tool.execute.after` |

Five OpenCode surfaces collapse onto a single hya `chat.params` registration.
Both `shell.env` and `tool.execute.before` share hya `tool.execute.before`.

---

## Shutdown and dispose

On `shutdown`, **before** replying and exiting, the adapter awaits each loaded
plugin’s `dispose` function in **reverse registration order** (last loaded
disposes first).

- A dispose that **throws** is logged to stderr and does **not** abort remaining
  dispose calls or the shutdown reply.
- Host-side `SHUTDOWN_TIMEOUT` is **1 s**; a slow dispose can still get the
  process killed by the host after that window (see
  [plugin protocol](plugin-protocol.md#limits-and-timeouts)).

---

## Plugin discovery order

At initialize (when not in `COMPAT_PURE` mode), the adapter builds a plugin
spec list in this order
([`loader/discovery.ts`](../crates/hya-plugin-compat/adapter/src/loader/discovery.ts),
[`loader/config_dirs.ts`](../crates/hya-plugin-compat/adapter/src/loader/config_dirs.ts)):

1. **Global config dir** — files `config.json`, `opencode.json`, `opencode.jsonc`,
   plus every `.js`/`.ts` under that dir’s `plugin/` and `plugins/` subdirectories.
2. **`$COMPAT_CONFIG`** — explicit config file (`customConfigFile`).
3. **Project ancestor configs** — walking from the worktree boundary **down to**
   cwd: each ancestor’s `opencode.json` and `opencode.jsonc` (unless
   `COMPAT_DISABLE_PROJECT_CONFIG` is set).
4. **Compat config dirs** (after global is peeled off for step 1) — for each of
   project `.opencode` dirs (cwd → worktree), `~/.opencode`, and optional
   `$COMPAT_CONFIG_DIR`: their `opencode.json` / `opencode.jsonc` plus every
   `.js`/`.ts` under `plugin/` and `plugins/`.
5. **`$COMPAT_CONFIG_CONTENT`** — inline JSON config.

**Precedence:** later duplicates **win**, via reverse dedup on package identity
(`file://` URL or npm package name).

### Config directories

| Kind | Path |
| --- | --- |
| Global | `$XDG_CONFIG_HOME/compat`, else `~/.config/compat` |
| Project | `<ancestor>/.opencode` for every directory from cwd up to the worktree boundary |
| Home | `~/.opencode` |
| Extra | `$COMPAT_CONFIG_DIR` when set |

Environment variables that steer discovery are listed in
[Configuration](configuration.md) (Compat adapter section): `COMPAT_CONFIG`,
`COMPAT_CONFIG_DIR`, `COMPAT_CONFIG_CONTENT`, `COMPAT_DISABLE_PROJECT_CONFIG`,
`COMPAT_PURE`, `HYA_DIRECTORY`, `HYA_WORKTREE`, `HYA_COMPAT_OPTIONS_JSON`, etc.

---

## Plugin factory input

Each plugin factory is called with:

| Field | Meaning |
| --- | --- |
| `client` | SDK client shim |
| `directory` | Working directory (`HYA_DIRECTORY` or cwd) |
| `worktree` | Worktree boundary (`HYA_WORKTREE` or directory) |
| `project` | Compat project object |
| `serverUrl` | URL from `HYA_SERVER_URL` (default `http://127.0.0.1:0`) |
| `$` | Bun shell |
| `experimental_workspace` | `{ register(type, adapter) }` |

Adapters registered through `experimental_workspace.register` whose adapter
object carries string `name` and `description` are collected into the
initialize reply’s `workspaceAdapters` array and surface at
`GET /experimental/workspace/adapter`. Adapters missing those string fields are
**dropped**.

---

## Plugin module shapes and resolution

### Accepted module shapes

**(a) v1** — default export with `id` / `server` / `tui` keys:

- `server` **must** be a function when present.
- A **tui-only** module (tui function, no server) loads as **zero hooks**.
- A module with **both** `server` and `tui` is an **error** (`mixed server and tui exports`).

**(b) Legacy** — every export is either a function or a `{ server }` record;
each becomes a server plugin entry. Non-function exports that are not
`{ server }` fail the module.

### Local path resolution

`file://`, `./relative`, and absolute specs resolve against the **config file’s
directory** (not cwd). A directory resolves to itself if it contains
`package.json`, otherwise to `index.ts` / `index.tsx` / `index.js` /
`index.mjs` / `index.cjs`, otherwise `PluginPathResolutionError`.

### npm resolution

Walks `node_modules` upward from the config file directory (or cwd). Prefers
`exports["./server"]`, then `main`. Raises `NpmPluginPackageError` if the
resolved entry escapes the package directory.

### Silently dropped specs

Any spec containing `compat-openai-codex-auth` or `compat-copilot-auth` is
**silently skipped** before loading
([`isDeprecatedPluginSpec`](../crates/hya-plugin-compat/adapter/src/loader/package.ts)).

---

## Hook behavior quirks

### `shell.env` only applies to the `shell` tool

`shell.env` hooks run only when the intercepted tool is literally named
`"shell"`. The collected env map is merged **over** `input.env`. An `Error`
thrown by a `shell.env` hook is **swallowed**.

### Throwing `tool.execute.before` becomes a veto

A compat `tool.execute.before` hook that **throws** is translated on the hya
wire into:

```json
{ "outcome": "veto", "reason": "<error message>" }
```

Throwing **blocks** the tool rather than being ignored.

### `permission.ask` status mapping

| Compat `status` | hya outcome |
| --- | --- |
| `allow` | `allow_once` |
| `deny` | `reject` (**no** `feedback` field) |
| `ask` | `defer` |

The compat path **never** produces `allow_always`, so “always allow” from a
compat plugin is impossible.

---

## Tools

- The adapter collects `hook.tool.<name>` definitions. The **first**
  registration of a name wins; later duplicates are ignored.
- `inputSchema` is derived from Zod `args` via `z.toJSONSchema`, or taken from
  a raw JSON-schema record (normalized to `{ type: "object", … }`).
- Results are normalized to `{ title, output, metadata, attachments? }`.
- **`ctx.ask()` is not supported** and throws `UnsupportedToolAskError` —
  compat tools cannot prompt the user.

---

## Event conversion

Only the following hya envelope event types are translated to Compat events.
**Anything not listed is silently dropped** before reaching the plugin:

| Envelope `event.type` |
| --- |
| `session_created` |
| `session_titled` |
| `command_executed` |
| `message_started` |
| `message_finished` |
| `text_start` |
| `text_delta` |
| `text_replace` |
| `text_end` |
| `reasoning_start` |
| `reasoning_delta` |
| `reasoning_end` |
| `tool_input_start` |
| `tool_input_delta` |
| `tool_call_requested` |
| `tool_result` |
| `tool_error` |
| `error` |

---

## Related

- [Plugin protocol](plugin-protocol.md) — NDJSON JSON-RPC wire the adapter implements
- [Configuration](configuration.md) — `kind: compat`, adapter env vars, discovery knobs
- [Compat parity](compat-parity.md) — broader surface coverage notes
